;; A minimal Component Model component that exports the
;; `qos:format-provider/format-provider` interface. Every entry point spins
;; forever, so the host's epoch-based wallclock budget kills it. We hand-write
;; the WAT to avoid a second cargo build in the test pipeline.
;;
;; Compiled with wasmtime's `wat::parse_str` at test setup time, then loaded
;; with `LoadedComponent::from_bytes`.
(component
  ;; Type aliases mirroring the WIT records on the wire.
  (type $confidence (record (field "tier" u32) (field "score" f32)))
  (type $detected   (record (field "confidence" $confidence) (field "mime" (option string))))
  (type $codec-error (record (field "kind" string) (field "message" string)))

  (core module $m
    (memory (export "memory") 1)

    ;; `detect` lifted ABI: head pointer, head len, return-area pointer.
    (func (export "detect") (param i32 i32 i32)
      (loop $L (br $L))
    )
    ;; `import-doc` lifted ABI: same shape (one list<u8> param, result<list<u8>, codec-error>).
    (func (export "import-doc") (param i32 i32 i32)
      (loop $L (br $L))
    )
    (func (export "export-doc") (param i32 i32 i32)
      (loop $L (br $L))
    )
    (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32)
      i32.const 65536
    )
  )

  (core instance $i (instantiate $m))

  (alias core export $i "memory"        (core memory $mem))
  (alias core export $i "cabi_realloc"  (core func $realloc))

  ;; Bind the WIT exports onto the core funcs.
  (func (export "qos:format-provider/format-provider#detect")
        (param "head" (list u8)) (result $detected)
    (canon lift (core func $i "detect")
                (memory $mem) (realloc $realloc))
  )
  (func (export "qos:format-provider/format-provider#import-doc")
        (param "bytes" (list u8)) (result (result (list u8) (error $codec-error)))
    (canon lift (core func $i "import-doc")
                (memory $mem) (realloc $realloc))
  )
  (func (export "qos:format-provider/format-provider#export-doc")
        (param "canonical" (list u8)) (result (result (list u8) (error $codec-error)))
    (canon lift (core func $i "export-doc")
                (memory $mem) (realloc $realloc))
  )
)
