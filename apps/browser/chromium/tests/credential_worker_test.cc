#include "credential_worker.h"
#include <cassert>
#include <condition_variable>
#include <mutex>
#include <thread>

struct State {
  std::mutex mutex;
  std::condition_variable cv;
  bool entered=false, release=false;
  int calls=0, completed=0;
  std::thread::id thread;
};
class Store final : public qos::CredentialStore {
 public:
  explicit Store(std::shared_ptr<State> state):state_(std::move(state)) {}
  qos::CredentialResult Execute(qos::CredentialRequest request) override {
    std::unique_lock lock(state_->mutex);
    ++state_->calls;state_->thread=std::this_thread::get_id();state_->entered=true;state_->cv.notify_all();
    state_->cv.wait(lock,[&]{return state_->release;});
    qos::CredentialResult result;result.status=qos::CredentialStatus::Ok;
    result.secret=std::move(request.secret);
    return result;
  }
 private: std::shared_ptr<State> state_;
};
static qos::CredentialRequest Request() {
  qos::CredentialRequest request;
  request.operation=qos::CredentialOperation::Fill;
  request.origin="https://example.com";request.account="synthetic-account";
  request.secret=qos::CredentialSecret("synthetic-secret");
  return request;
}
int main() {
  qos::CredentialCancellation queued(Request());
  queued.Cancel();
  assert(!queued.TakeRequest()); // cancellation destroys queued payload now
  qos::CredentialCancellation started(Request());
  auto taken=started.TakeRequest();
  assert(taken && taken->secret.View()=="synthetic-secret");
  started.Cancel();
  assert(started.Cancelled());
  assert(!started.TakeRequest());

  auto state=std::make_shared<State>();
  qos::CredentialWorker private_worker(true,std::make_unique<Store>(state));
  assert(!private_worker.Submit(Request(),[](auto,auto){}));
  assert(state->calls==0);
  qos::CredentialWorker worker(false,std::make_unique<Store>(state));
  auto first=worker.Submit(Request(),[state](auto result,bool cancelled){
    assert(cancelled);assert(result.status==qos::CredentialStatus::Ok);
    std::lock_guard lock(state->mutex);++state->completed;state->cv.notify_all();
  });
  assert(first);
  {
    std::unique_lock lock(state->mutex);
    assert(state->cv.wait_for(lock,std::chrono::seconds(5),[&]{return state->entered;}));
    assert(state->thread!=std::this_thread::get_id());
  }
  std::vector<std::shared_ptr<qos::CredentialCancellation>> pending;
  for(size_t i=1;i<qos::CredentialWorker::kCapacity;++i) {
    auto token=worker.Submit(Request(),[state](auto result,bool cancelled){
      assert(cancelled);assert(result.status==qos::CredentialStatus::Cancelled);
      std::lock_guard lock(state->mutex);++state->completed;state->cv.notify_all();
    });
    assert(token);pending.push_back(token);
  }
  assert(!worker.Submit(Request(),[](auto,auto){}));
  first->Cancel();for(auto& token:pending)token->Cancel();
  {
    std::lock_guard lock(state->mutex);state->release=true;state->cv.notify_all();
  }
  {
    std::unique_lock lock(state->mutex);
    assert(state->cv.wait_for(lock,std::chrono::seconds(5),[&]{return state->completed==16;}));
    assert(state->calls==1); // queued cancellation never calls the store
  }
  worker.Stop();
  assert(!worker.Submit(Request(),[](auto,auto){}));
  auto closing=std::make_shared<State>();
  {
    qos::CredentialWorker closing_worker(false,std::make_unique<Store>(closing));
    assert(closing_worker.Submit(Request(),[closing](auto result,bool cancelled){
      assert(cancelled);assert(result.status==qos::CredentialStatus::Ok);
      std::lock_guard lock(closing->mutex);++closing->completed;closing->cv.notify_all();
    }));
    std::unique_lock lock(closing->mutex);
    assert(closing->cv.wait_for(lock,std::chrono::seconds(5),[&]{return closing->entered;}));
    // Destructor must return while the fake OS call remains blocked.
  }
  {
    std::lock_guard lock(closing->mutex);closing->release=true;closing->cv.notify_all();
  }
  {
    std::unique_lock lock(closing->mutex);
    assert(closing->cv.wait_for(lock,std::chrono::seconds(5),[&]{return closing->completed==1;}));
  }

}
