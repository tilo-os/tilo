# Validation v1 failed

Scoped format/diff check passed. Strict production Clippy failed with E0609: generated Shaders lacked the original clip_leaf field. A permutations group replaces the implicit base shader, so the new clip_leaf group must also explicitly list + clip_leaf. Root missed this in initial static review; the compiler caught it. The original failure log and generated-artifact metadata are retained. No GPU test, release build, or timing ran for v1.

After the command terminated, Terra was asked for the one-line registration repair. No default, workload, shader core, tolerance, timer or recovery change is authorized. v2 must rerun the full plan and preserve all 46 prior shader tokens/metadata before timing.
