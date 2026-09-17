#pragma once
#include "credential_document.h"
#include <atomic>
#include <condition_variable>
#include <deque>
#include <functional>
#include <memory>
#include <mutex>
#include <string_view>
#include <thread>
#include <vector>

namespace qos {
// Move-only owned buffer. OS/CEF copies require their own lifetime management;
// wiping this allocation is not a claim of perfect process-memory erasure.
class CredentialSecret {
 public:
  CredentialSecret()=default;
  explicit CredentialSecret(std::string_view value):bytes_(value.begin(),value.end()) {}
  CredentialSecret(const CredentialSecret&)=delete;
  CredentialSecret& operator=(const CredentialSecret&)=delete;
  CredentialSecret(CredentialSecret&& other) noexcept:bytes_(std::move(other.bytes_)) {}
  CredentialSecret& operator=(CredentialSecret&& other) noexcept {
    if(this!=&other){Clear();bytes_=std::move(other.bytes_);}return *this;
  }
  ~CredentialSecret(){Clear();}
  std::string_view View() const {return {bytes_.data(),bytes_.size()};}
 private:
  void Clear(){volatile char* data=bytes_.data();for(size_t i=0;i<bytes_.size();++i)data[i]=0;bytes_.clear();}
  std::vector<char> bytes_;
};

enum class CredentialStatus { Ok, Cancelled, NotFound, Denied, Unavailable, Invalid, Exists, Capacity };
struct CredentialAccount {std::string id, username;};
struct CredentialRequest {
  CredentialOperation operation=CredentialOperation::List;
  std::string origin, account, username;
  CredentialSecret secret;
  // Native-only cancellation between OS calls; never serialized.
  std::function<bool()> cancelled;
};
struct CredentialResult {
  CredentialStatus status=CredentialStatus::Unavailable;
  std::vector<CredentialAccount> accounts;
  CredentialSecret secret;
};
class CredentialStore {
 public:
  virtual ~CredentialStore()=default;
  virtual CredentialResult Execute(CredentialRequest request)=0;
};
class CredentialCancellation {
 public:
  explicit CredentialCancellation(CredentialRequest request):request_(std::move(request)) {}
  void Cancel(){
    std::lock_guard lock(mutex_);cancelled_.store(true);request_.reset();
  }
  bool Cancelled() const{return cancelled_.load();}
  // Taking and cancelling race under the same lock. Queued payloads are wiped
  // immediately on cancellation; an already-taken request belongs to the OS job.
  std::optional<CredentialRequest> TakeRequest(){
    std::lock_guard lock(mutex_);
    return std::exchange(request_,std::nullopt);
  }
 private:
  std::mutex mutex_;
  std::optional<CredentialRequest> request_;
  std::atomic<bool> cancelled_{false};
};

// All store calls run serially off CEF UI/IO/render threads. Completion runs on
// this worker; the broker must marshal to UI and revalidate the document there.
// An in-flight OS mutation cannot be undone by cancellation: its actual result
// is delivered with cancelled=true for reconciliation, never reported as an
// unperformed operation. Stop never waits for an unresponsive OS Keychain call.
class CredentialWorker {
 public:
  static constexpr size_t kCapacity=16;
  using Completion=std::function<void(CredentialResult,bool)>;
  CredentialWorker(bool private_mode,std::unique_ptr<CredentialStore> store)
      :private_mode_(private_mode),state_(std::make_shared<State>(std::move(store))) {
    if(!private_mode_)std::thread([state=state_]{Run(state);}).detach();
  }
  CredentialWorker(const CredentialWorker&)=delete;
  CredentialWorker& operator=(const CredentialWorker&)=delete;
  ~CredentialWorker(){Stop();}
  std::shared_ptr<CredentialCancellation> Submit(CredentialRequest request,Completion completion) {
    if(private_mode_ || !completion || !Valid(request))return {};
    std::lock_guard lock(state_->mutex);
    if(state_->stopped || !state_->store || state_->outstanding>=kCapacity)return {};
    auto cancellation=std::make_shared<CredentialCancellation>(std::move(request));
    state_->queue.push_back({std::move(completion),cancellation});
    ++state_->outstanding;state_->cv.notify_one();return cancellation;
  }
  void Stop() {
    std::lock_guard lock(state_->mutex);state_->stopped=true;
    for(auto& job:state_->queue)job.cancellation->Cancel();
    if(state_->active)state_->active->Cancel();
    state_->cv.notify_all();
  }
 private:
  struct Job {Completion completion;std::shared_ptr<CredentialCancellation> cancellation;};
  struct State {
    explicit State(std::unique_ptr<CredentialStore> value):store(std::move(value)){}
    std::mutex mutex;std::condition_variable cv;
    std::unique_ptr<CredentialStore> store;
    std::deque<Job> queue;
    std::shared_ptr<CredentialCancellation> active;
    size_t outstanding=0;bool stopped=false;
  };
  static bool Valid(const CredentialRequest& request) {
    return request.origin.starts_with("https://") && request.origin.size()<=8192 &&
           request.account.size()<=128 && request.username.size()<=1024 &&
           request.secret.View().size()<=4096;
  }
  static void Run(const std::shared_ptr<State>& state) {
    for(;;) {
      std::optional<Job> job;
      {
        std::unique_lock lock(state->mutex);
        state->cv.wait(lock,[&]{return state->stopped || !state->queue.empty();});
        if(state->queue.empty())return;
        job.emplace(std::move(state->queue.front()));state->queue.pop_front();
        state->active=job->cancellation;
      }
      CredentialResult result;
      auto request=job->cancellation->TakeRequest();
      if(!request)result.status=CredentialStatus::Cancelled;
      else {
        request->cancelled=[cancellation=job->cancellation]{return cancellation->Cancelled();};
        result=state->store->Execute(std::move(*request));
      }
      job->completion(std::move(result),job->cancellation->Cancelled());
      {
        std::lock_guard lock(state->mutex);state->active.reset();--state->outstanding;
      }
    }
  }
  const bool private_mode_;
  std::shared_ptr<State> state_;
};
} // namespace qos
