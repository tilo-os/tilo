# Sign-in: one secret, several ways to give it

*2026-10-24*

A person types one secret per boot, at a screen that looks like the login screen,
and it is the same secret everywhere else Tilo asks who they are. Their fingerprint,
a security key or an approval from another of their devices can stand in for it
wherever that is safe, and the password always works when they don't. No machine
ever asks for a disk secret and then a login password.

It is the design behind [architecture.md](architecture.md) §3.6. The rule on
strength is the same as there: a short secret only where the TPM throttles guesses.

The model is the one Android and ChromeOS converged on, with the macOS fallback for
machines that cannot unlock their system by themselves:

- **The system unlocks by itself when the boot is verified.** The TPM releases the
  system volume's key only to a boot it can prove is an approved Tilo. The person
  lands on the login screen without having typed anything.
- **A person's data unlocks with their login.** Everything private to a person is
  encrypted under a key that only their login releases. A thief who boots the
  machine reaches the login screen and nothing else.
- **When the boot can't be verified, the login screen comes first.** On a machine
  without a TPM, or one whose boot isn't verified, the first screen *is* the login
  screen. The one secret typed there opens the system and the person's data and
  signs them in, without a second prompt.

---

## 1. What has to be true

1. **One secret per person.** Booting, signing in, unlocking the screen, approving a
   privileged action and opening saved passwords never ask for two different
   secrets, and never ask for the same one twice in a row.
2. **The password is the fallback that always works.** Every other factor can fail
   (no reader, flat key, phone out of reach); the password never stops working
   because of it. The recovery key stays the guarantee behind the password.
3. **A short secret only where guessing is throttled.** A six-digit PIN is fine when
   the TPM limits guesses to about a dozen a day. Without that, the secret must
   carry its own entropy. This rule is unchanged from §3.6.
4. **Nothing personal is readable before its owner signs in.** Not after a reboot,
   not to someone who has taken the disk out, not to another local user.
5. **Customizing doesn't cost the guarantees.** A developer who boots their own
   kernels keeps verified boot, automatic system unlock and the short secret,
   because they sign what they boot (§7). Turning verification off is possible,
   is shown plainly, and costs only what it must (§7.4).
6. **Recovery never depends on the thing that broke.** A forgotten password, a
   cleared TPM, a lost security key or a lost phone each has a way back that does
   not involve it.

## 2. Two scopes, two kinds of storage

| | Device storage | Personal storage |
|---|---|---|
| Holds | the OS's state: `/etc`, `/var/lib` system state, Wi-Fi networks shared by everyone, the device identity, update and rollback state | a person's home, rooms, saved passwords, account identity share, private Wi-Fi networks, browser profiles |
| Encrypted by | the system volume key | that person's **user key** |
| Available | from boot | from that person's sign-in until shutdown (or sleep, if they choose, §6.3) |
| Released by | the TPM, to a verified boot; otherwise any disk-unlocking person's user key | that person's factors (§4) |

Everything that runs before anyone signs in — networking, updates, sync relaying,
the login screen itself — may use device storage only. A component that needs a
person's data waits for their sign-in, as on Android where only "device-encrypted"
storage exists until the first unlock.

### 2.1 Where things live

- **Homes** become per-person encrypted volumes (§5.1).
- **The vault** is keyed from the person's user key, not from the device key. The
  device key no longer protects anyone's secrets; it identifies the machine.
- **The room store** is unlocked by its owner's user key instead of a key file on
  the root. Before its owner signs in, the machine still relays ciphertext for
  collaborators but cannot read or serve room contents.
- **Wi-Fi networks** a person adds are personal by default; a network the admin
  marks "for everyone" stays in device storage so updates and remote unlock can
  use it before sign-in.
- **The TPM lockout authorisation** is no longer a file on the root (§6.4).
- **The device key** moves into the TPM where one exists, so a compromise of the
  running system before sign-in can use it but not copy it.

## 3. Booting

### 3.1 Verified boot: the system unlocks itself

The system volume gets a TPM keyslot with no PIN. Its policy is the rollback policy
Tilo already uses, without the authorisation value:

```
PolicyPCR(11) → PolicyAuthorize(authority, generation) → PolicyPCR(7) → PolicyNV(floor)
```

PCR 11 carries the UKI, its command line and the composefs digest; PCR 7 carries
the Secure Boot state and keys. So the keyslot opens only for a UKI the authority
signed a prediction for, booted with Secure Boot on under this machine's keys, at or
above the rollback floor. The token format already supports this (`pin: false`).
The repair command line measures differently, so repair mode never opens this slot
and keeps its own gate ([repair-mode.md](repair-mode.md)).

"Verified" here means all of: Secure Boot on, the db holding only keys this machine
trusts (Tilo's release key, the owner's key, or both), kernel lockdown active, and a
signed prediction for the booted generation. Microsoft's third-party CA in db breaks
the condition, because anything it signed could then ask the TPM for the key.

The person then sees the ordinary login screen.

### 3.2 Unverified boot or no TPM: the login screen comes first

When the system volume can't open by itself, the boot shows the login screen in the
initrd: the same layout, the same account pictures, the same factors as the real
one. The secret typed there:

1. releases the person's user key (§4),
2. opens the system volume through that person's keyslot, a LUKS slot whose key is
   derived from the user key (every person allowed to unlock the machine has one;
   the admin decides who),
3. is handed to the session as a one-time, in-memory sign-in for that person and
   that boot only, so the login screen after switch-root signs them straight in.

The handoff is a kernel keyring entry owned by root, created in the initrd, read
once by the greeter's PAM service and destroyed. It never touches disk. If anything
in the chain fails, the greeter simply asks again; it never signs anyone in on a
stale or foreign handoff.

On a machine with a TPM, even an unverified boot keeps the short secret: the PIN
still goes to the TPM, which throttles it no matter which system asks (§4.1). What
the unverified boot loses is protection against someone who modifies the boot to
capture the PIN (§7.4).

On a machine without a TPM, the secret must be a strong passphrase.

The initrd draws each person's face as the login screen does before anyone signs
in: the initials of their name in the colour their account name derives
(`tilo-face`), in the login screen's colours and sizes (`look.json`, which the
greeter's own test keeps current). It shows no account picture. A picture would
have to sit in the volume header beside the name, readable by anyone holding the
disk, and the header carries only what the list of people needs: uid, account
name, display name and the kind of secret. Plymouth draws text in the one font the
initrd carries, at regular weight, so names and messages there are set in that
font rather than the login screen's.

### 3.3 What the person sees

| Machine | First screen | Secrets typed |
|---|---|---|
| TPM, verified boot | login screen, after the system unlocked itself | one: PIN or password |
| TPM, boot not verified | login screen in the initrd | one: PIN or password |
| No TPM | login screen in the initrd | one: passphrase |
| Anything, factors exhausted | the same screen, offering the recovery key | the recovery key, then set a new secret |

## 4. The user key and its wrappers

Each person has a random 256-bit **user key**, created with their account. It is
the root of their personal scope: it opens their home, keys their vault and room
store, and (if they may unlock the machine) derives their system-volume keyslot.
It is never stored in the clear. It is stored several times, each copy wrapped by
one factor, in the person's record in device storage:

| Wrapper | Unwraps with | Throttled by | First sign-in after boot |
|---|---|---|---|
| **TPM + secret** | PIN or password, as the authorisation value of a TPM-sealed object | the TPM's dictionary-attack lockout | yes |
| **Passphrase** | Argon2id of the passphrase (only on machines without a TPM) | nothing; the passphrase must be strong | yes |
| **Security key** | FIDO2 `hmac-secret` from an enrolled key, with its PIN or touch | the key itself | yes |
| **Another device** | a share released by one of the person's devices after they approve | the approving device | yes |
| **Recovery key** | the person's recovery key | its 256 bits | yes |
| **Fingerprint** | — | — | **no** (§4.3) |

### 4.1 Why the TPM wrapper isn't bound to the boot

The system volume's keyslot is bound to the boot because it has no secret: binding is
the only thing protecting it. The TPM + secret wrapper has a secret, and the TPM
throttles guesses at it whichever system asks, so a thief who boots their own system
gets the same dozen guesses a day as one at the login screen. Binding it to PCRs would
add nothing against guessing. It would stop the short secret from working on an
unverified boot, which is exactly the case where a developer still wants one secret
(§7.4). So the wrapper's policy is the authorisation value alone. Dictionary-attack
protection stays on.

### 4.2 Every check asks the TPM

A six-digit PIN hashed in `/etc/shadow` is cracked in seconds by anyone who can read
the system volume. That includes a compromise of the running system before sign-in,
which is now a system with its volume already open. So no hash of the secret is
stored anywhere: accounts keep a locked shadow entry, and every check — sign-in, lock
screen, privileged prompt — verifies the secret by unwrapping the user key through
the TPM. There is one way a secret is checked, and tools that change or verify it
(`passwd`, `su`, AccountsService) go through `pam_tilo` like everything else.

The TPM is the only limit on guessing, and the only one that matters against an
attacker who can run their own code against it. Its two parameters do different
jobs. `recoveryTime` caps sustained guessing: at two hours, twelve guesses a day, so
about a century on average for a six-digit PIN. `maxTries` only sizes the burst
before that cap, so it is set for the owner rather than the attacker: Tilo asks for
10, about where phones start making people wait, and a few extra guesses are noise
against a million PINs. On a TPM that
Windows manages, its parameters apply, and `tilo-rollback tpm-status` reports
whether they meet §1.3.

One rule spares the owner's tries: **an entry already tried is not tried again.**
Someone who isn't sure the first attempt registered and types the same thing twice
spends one try, not two. `tilo-authd` keeps the person's recent wrong entries as HMACs
under a key generated at boot, in its own memory only. They are cleared on success and
after ten minutes, and never reach disk. The rule is no security boundary — anything
that bypasses `tilo-authd` still meets the TPM — so it only has to be correct.

The screen always says what is happening, in plain words: how many tries are left
before a wait, when the next try is possible, and the other ways in (security key,
another device, the recovery key).

### 4.3 What fingerprints can and can't do

No fingerprint stack on Linux releases a secret: the sensor (or fprintd) answers
"match" or "no match", and a "match" can be forged by anything that can talk to the
daemon. So a fingerprint can only confirm a person whose user key is already in
memory: the lock screen, privileged-action prompts, saved-password prompts. After a
reboot, a sign-out or a key-evicting sleep (§6.3), the person gives a factor that
can unwrap. macOS and Windows behave the same way after a restart.

Face sign-in works the same way when there is a driver for the camera. The Yoga
Slim 7x's IR camera has none yet.

### 4.4 Approval from another device

The pieces exist: devices certified by the same Tilo identity, a signed approval
type for unlocking a session (`tilo-user-identity` `SessionUnlock`), and the boot-time
sibling unlock that already asks another device to release half of a disk key. The
"another device" wrapper generalises that:

- The user key is split into a local half, sealed to this machine's TPM, and a remote
  half held by each device the person chose to approve from.
- At the login or lock screen, "Approve on another device" sends a signed request.
  The approving device shows which machine is asking, a short code that both screens
  display, and what will unlock.
- On approval it returns its half, encrypted to the requesting machine's device key.
  The request is single-use and bound to that screen.
- A phone approves through the Tilo web desktop with a passkey, so no separate app is
  needed.

The machine must have network before sign-in, which it does only through networks
marked "for everyone".

### 4.5 The recovery key

Every person gets a recovery key, shown once when their account is created, as now.
It unwraps their user key, so it also opens the system volume for anyone allowed to
unlock it. The first account's recovery key is the machine's recovery key. Using it
always ends with setting a new secret.

## 5. Implementation shape

### 5.1 Homes: systemd-homed as the container, Tilo as the key

`systemd-homed`'s LUKS backend gives each person an encrypted home that mounts at
sign-in, unmounts at sign-out, and can drop its key on sleep. It is maintained, it is
integrated with PAM, and it handles resizing and trim. What it does not do is ask a
TPM. So Tilo gives homed a **high-entropy home secret derived from the user key**, and
homed never sees the person's PIN. Homed provides the container; Tilo releases the
key. Changing the PIN re-wraps the user key and never touches homed.

The home lives inside the system volume, so a home is encrypted twice on machines
whose system volume is encrypted. AES on the Snapdragon and on any x86 machine Tilo
supports runs at several GB/s per core. A storage benchmark against the single-layer
layout gates the switch, under the perf bar that applies to every change.

Until then the PAM services carry no `pam_systemd_home` lines; they come back with
personal storage (§8).

### 5.2 `tilo-authd` and one PAM service

A small root service owns every user record (the user key's wrappers, enrolled
factors, approving devices) and does every unwrap. One PAM module, `pam_tilo`, asks it
to authenticate a person by any of their factors. The greeter, the lock screen helper,
polkit, the privileged-access tool and the saved-password prompt all use one PAM
service built from that module, so every surface accepts the same factors and none
carries its own stack.

That service is `/etc/pam.d/tilo-auth`. `greetd`, `tilo-unlock`, `polkit-1` (which
polkit's agent helper uses) and `sudo` take their auth, account and password phases
from it and keep only their own session phase. `run0` has no PAM authentication of
its own; it asks polkit. Hosts without `tilo-authd` (the distribution packages) keep
their own unlock stack.

The fingerprint step runs only when `tilo-authd` answers that it holds the person's
key for a session of theirs running now (§4.3). The daemon holds a key from the
sign-in that unwrapped it until that person's sessions end. So the greeter after
boot, the first sign-in and any screen after the key was dropped never offer a
fingerprint, and fprintd's "match" can only confirm someone already signed in. The
greeter asks the same question before showing its fingerprint button. Moving the
fingerprint request itself into `tilo-authd` comes later.

Factor-specific hardware stays in its own daemon: fprintd for fingerprints, libfido2
for keys, the TPM through `/dev/tpmrm0`. `tilo-authd` asks them and never parses
their protocols itself.

### 5.3 What each screen offers

| Factor | Login screen (first after boot) | Login screen (later) | Lock screen | Privileged prompt |
|---|---|---|---|---|
| PIN / password | yes | yes | yes | yes |
| Security key | yes | yes | yes | yes |
| Another device | yes | yes | yes | yes, with the action named on the approving device |
| Fingerprint | no | only while the person's key is in memory (switching users) | yes | yes |
| Recovery key | yes, under "Can't sign in" | yes | no: sign out first | no |

A factor that isn't enrolled or whose hardware isn't present isn't shown. The
password field is always there.

## 6. Running

### 6.1 Before anyone signs in

The machine is booted with device storage open and no personal data available. That
is more exposure than a machine waiting at a disk prompt, and it is bounded on purpose:

- New USB devices other than keyboards and pointers are not bound while nobody is
  signed in or the session is locked. Thunderbolt and USB4 devices stay behind the
  IOMMU and are not authorised.
- The login screen is the only interactive surface. Remote access and the developer
  control sockets don't exist until someone signs in.
- Nothing readable at this point is personal (§2), and the device key can be used
  but not copied (§2.1).

### 6.2 Multiple people

Each person has their own user key, home, vault and factors. A person the admin
allows to unlock the machine gets a system-volume keyslot; others can sign in only
after the system is up. The TPM's dictionary-attack counter is shared by everyone
on the machine, and with Windows on a shared TPM ([rollback.md](rollback.md)). With
10 tries that rarely matters; when it does, the screen says that the wait comes from
this computer's shared limit, not from the person's own attempts.

### 6.3 Locking and sleep

By default a locked or sleeping session keeps the person's key in memory, so a
fingerprint unlocks it. "Require my password after sleep" (Settings, Sign-in) drops
the home's key and the user key on suspend, as homed's lock-on-suspend does, so a
sleeping laptop holds no personal key. Waking it then takes a factor that can unwrap.

### 6.4 The TPM lockout authorisation

A file on the system volume would be readable by a compromise before sign-in, which
could then reset the lockout counter and guess PINs without limit. So it is stored wrapped by the machine's recovery key and by the admin's user key, never in
the clear. Resetting a lockout needs either of them, which is exactly when they are at
hand: a locked-out person signs in with a security key, another device or the recovery
key, and Tilo resets the counter for them. On a TPM shared with Windows, Windows
keeps it ([rollback.md](rollback.md)).

### 6.5 Automatic sign-in

Settings keeps "Sign in automatically" for the admin. With this design it means
"personal data is readable by anyone who turns the machine on", and the toggle says
that. It is never on by default and never turns on by itself.

## 7. Development machines

A developer boots kernels, images and system extensions they built themselves. The
guarantees are the same as for everyone else — the system unlocks itself only for a
boot the machine approved, and a thief can't change what boots — because on a
development machine the **owner approves what boots**.

### 7.1 The owner authority

Each machine can have an owner authority: a signing key held by the TPM, usable only
after a fresh confirmation by the admin (any factor that confirms them). It signs
three things:

1. **Secure Boot.** The owner's db key, as `tilo-secureboot` already enrolls, signs
   UKIs and kernels.
2. **PCR 11 predictions.** The system volume's TPM keyslot authorises predictions
   from the owner authority instead of Tilo's release key.
3. **System extensions**, as verity signatures the kernel checks before merging.

Releases keep working with no extra step: when an update arrives, the machine checks
the release signature and prediction against Tilo's release key, then counter-signs
both with the owner authority. The policy therefore has one authority, the owner,
and a release can never boot unless the owner's machine accepted it.

### 7.2 What the developer does

`tilo device swap kernel`, an image upgrade from a local build, or installing a
system extension ends in one confirmation from the system, drawn by the compositor:
"Trust kernel 7.2.10+dev built today at 14:03 for booting this computer". The
developer confirms with their fingerprint, security key or PIN, and the artifact is
signed and its prediction issued. The next boot is verified, the system unlocks
itself, and they sign in with their one short secret.

Development images keep the UKI. A kernel that isn't part of the image and
extensions merged at runtime are covered by owner-signed UKIs and signed extensions,
so nothing on a development machine has to boot unmeasured.

### 7.3 What this protects and what it doesn't

Malware running as root on a development machine can't sign by itself, because the
authority asks the person and the prompt can't be drawn by anything but the
compositor. Malware that waits until the developer approves something can piggyback
on that approval. That is the same exposure as `sudo` running a kernel installer on
any distribution, and it is shown in the confirmation: what is being trusted, built
from which tree, when.

### 7.4 Turning verification off

A developer can still boot with Secure Boot off: to debug a firmware issue, or on a
machine whose firmware won't take owner keys. Then:

- The system volume doesn't unlock by itself, so the login screen comes first (§3.2).
- The short PIN still works, because the TPM still throttles it (§4.1).
- What is lost is evil-maid protection: someone with the machine for a few minutes can
  change what boots, and the changed system can capture the PIN the next time it's
  typed. The security overview says this in those words while it is the case.

So a development machine never needs two secrets. The choice is between "one short
secret, fully verified" and "one short secret, unverified boot"; Tilo recommends the
first and makes it the default when the firmware allows owner keys.

## 8. Order of work

Each step leaves every machine working and is useful on its own.

1. **`tilo-authd`, the user key and `pam_tilo`.** TPM + secret, passphrase and
   recovery wrappers. Greeter, lock screen and privileged prompts move to one PAM
   service. Shadow entries lock for TPM-wrapped accounts. The lockout authorisation
   moves into its wrappers.
2. **The boot-time login screen and the handoff.** A person's keyslot on the system
   volume, derived from the user key. This alone gets every current machine, verified
   or not, to one secret per boot.
3. **Personal storage.** Homed-backed homes, the vault and the room store re-keyed
   from the user key, personal Wi-Fi networks. Migration of existing homes (§9).
4. **Automatic system unlock** on verified boots, and the pre-sign-in hardening of
   §6.1.
5. **Factors.** Security keys at every screen; fingerprint at the lock screen and in
   prompts, with enrollment in Settings; approval from another device; one Sign-in
   page in Settings for all of them.
6. **The owner authority** and owner-signed development images.

## 9. Migrating an existing machine

Migration never deletes or rewrites data in place and never changes an account's id.
A person's home is copied into their new encrypted home while both exist. The copy is
verified, then the person is switched over, and the old directory is removed only once
they have signed in to the new one. Every step can be resumed after an interruption.
The old disk secret keeps working until the person's new keyslot has been proven to
open the volume, and the recovery key keeps working throughout.

## 10. Still to establish

- Whether the Yoga Slim 7x's firmware accepts owner Secure Boot keys (Setup Mode).
  If it doesn't, that machine runs §7.4 until it does.
- The cost of a homed LUKS home inside the encrypted system volume on the Snapdragon,
  measured before step 3 switches anyone.
- A boot-time login screen with the greeter's accessibility (screen reader, touch).
  Plymouth can draw the layout but has no screen reader, so the initrd either carries
  the greeter itself or this is a known gap until it does. Today it is the gap,
  recorded with what closing it takes in [defects.md](defects.md) P1-13.
- Whether the Windows-managed lockout parameters on a shared TPM still meet §1.3 for a
  six-digit PIN. `tilo-rollback tpm-status` decides per machine, and a machine that
  fails gets a longer PIN, not a weaker check.
