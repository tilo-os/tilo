;; A minimal Component Model component that exports the
;; `tilo:format-provider/format-provider` interface. Every entry point spins
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

    ;; Canonical ABI exports receive the list pointer/length and return a
    ;; pointer to the flattened result area. The functions never reach that
    ;; return, but the signature must still match the interface being lifted.
    (func (export "detect") (param i32 i32) (result i32)
      (loop $L (br $L))
      unreachable
    )
    ;; `import-doc` has the same parameter/result-pointer shape.
    (func (export "import-doc") (param i32 i32) (result i32)
      (loop $L (br $L))
      unreachable
    )
    (func (export "export-doc") (param i32 i32) (result i32)
      (loop $L (br $L))
      unreachable
    )
    ;; The fixture only needs one allocation for the input list before the
    ;; exported function spins. Return a suitably aligned address well inside
    ;; the first page so canonical lowering reaches `detect` instead of
    ;; trapping while copying its argument.
    (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32)
      i32.const 1024
    )
    (func (export "cabi_post_detect") (param i32))
    (func (export "cabi_post_import_doc") (param i32))
    (func (export "cabi_post_export_doc") (param i32))
  )

  (core instance $i (instantiate $m))

  (alias core export $i "memory"        (core memory $mem))
  (alias core export $i "cabi_realloc"  (core func $realloc))
  (alias core export $i "cabi_post_detect" (core func $post-detect))
  (alias core export $i "cabi_post_import_doc" (core func $post-import))
  (alias core export $i "cabi_post_export_doc" (core func $post-export))

  ;; Lift the core functions, then export them through a versioned interface
  ;; instance, matching the shape produced by wit-component.
  (func $detect
        (param "head" (list u8)) (result $detected)
    (canon lift (core func $i "detect")
                (memory $mem) (realloc $realloc) (post-return $post-detect))
  )
  (func $import-doc
        (param "bytes" (list u8)) (result (result (list u8) (error $codec-error)))
    (canon lift (core func $i "import-doc")
                (memory $mem) (realloc $realloc) (post-return $post-import))
  )
  (func $export-doc
        (param "canonical" (list u8)) (result (result (list u8) (error $codec-error)))
    (canon lift (core func $i "export-doc")
                (memory $mem) (realloc $realloc) (post-return $post-export))
  )

  (component $provider-shim
    (type (list u8))
    (type (record (field "tier" u32) (field "score" f32)))
    (import "import-type-confidence" (type (eq 1)))
    (type (option string))
    (type (record (field "confidence" 2) (field "mime" 3)))
    (import "import-type-detected" (type (eq 4)))
    (type (func (param "head" 0) (result 5)))
    (import "import-func-detect" (func (type 6)))
    (type (record (field "kind" string) (field "message" string)))
    (import "import-type-codec-error" (type (eq 7)))
    (type (result 0 (error 8)))
    (type (func (param "bytes" 0) (result 9)))
    (import "import-func-import-doc" (func (type 10)))
    (type (func (param "canonical" 0) (result 9)))
    (import "import-func-export-doc" (func (type 11)))

    (type (record (field "tier" u32) (field "score" f32)))
    (export "confidence" (type 12))
    (type (option string))
    (type (record (field "confidence" 13) (field "mime" 14)))
    (export "detected" (type 15))
    (type (record (field "kind" string) (field "message" string)))
    (export "codec-error" (type 17))
    (type (list u8))
    (type (func (param "head" 19) (result 16)))
    (export "detect" (func 0) (func (type 20)))
    (type (result 19 (error 18)))
    (type (func (param "bytes" 19) (result 21)))
    (export "import-doc" (func 1) (func (type 22)))
    (type (func (param "canonical" 19) (result 21)))
    (export "export-doc" (func 2) (func (type 23)))
  )

  (instance $provider (instantiate $provider-shim
      (with "import-func-detect" (func $detect))
      (with "import-func-import-doc" (func $import-doc))
      (with "import-func-export-doc" (func $export-doc))
      (with "import-type-confidence" (type $confidence))
      (with "import-type-detected" (type $detected))
      (with "import-type-codec-error" (type $codec-error))
    )
  )
  (export "tilo:format-provider/format-provider@1.0.0" (instance $provider))
)
