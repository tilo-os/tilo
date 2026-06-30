@0xd9f8e5b8a1c44d10;

# qOS format-router RPC schema. The host exposes a single FormatRouter
# capability over a Unix-domain stream socket at
# $XDG_RUNTIME_DIR/qos/services/format-router.sock . Clients (FS daemon,
# apps) call detect/importAs/exportAs/list against this interface.

struct Confidence {
    tier  @0 :UInt32;
    score @1 :Float32;
}

struct Detected {
    confidence @0 :Confidence;
    mime       @1 :Text;
    providerId @2 :Text;
}

interface FormatRouter {
    detect    @0 (head :Data, hint :Text) -> (detected :Detected);
    importAs  @1 (bytes :Data, providerId :Text) -> (canonical :Data);
    exportAs  @2 (canonical :Data, providerId :Text) -> (bytes :Data);
    list      @3 () -> (providers :List(Text));
}
