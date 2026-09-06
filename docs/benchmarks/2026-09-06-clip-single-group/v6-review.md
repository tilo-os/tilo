# Validation v6

Only vendor lib.rs changed among the 34 inputs since v5. Root verified that both direct fixtures now use the typed 32-byte ClipElement at binding 4, matching WGSL alignment, and reviewed the remaining minimum binding sizes. The route proof now labels final settled_overflow=false explicitly; earlier recovery events remain logged. No production behavior, pixel comparison, timer or workload changed. All 20 validation checks are rerun and v1-v5 failures remain retained.
