//! Isolated public-module comparison, never production model admission.
//! Export/import names follow the exact publisher's Emscripten loader.
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::time::{Duration, Instant};
use wasmi::{
    Caller, Config, Engine, Extern, ExternType, Func, Instance, Linker, Memory, Module, Store,
    StoreLimits, StoreLimitsBuilder, Val,
};

const MODULE_SHA: &str = "c43f48e11f302087250d1e406024956781cd2f02595a5e343b3d7ddd5ef707fa";
const MODEL_SHA: &str = "b6e02f048568ac5d01a2042556c658061e699acbc0aa2a1439f52f3d461dffeb";
// The initial 2-billion-fuel stop is retained separately. This speech probe's
// ceiling remains subordinate to the unchanged external 180-second deadline.
const FUEL: u64 = 48_000_000_000;
type Failure = Box<dyn std::error::Error>;

struct Host {
    limits: StoreLimits,
    clock: Instant,
}

fn memory(caller: &Caller<'_, Host>) -> Result<Memory, wasmi::Error> {
    caller
        .get_export("q")
        .and_then(Extern::into_memory)
        .ok_or_else(|| wasmi::Error::new("missing declared memory"))
}

fn integer(values: &[Val], index: usize) -> Result<i32, wasmi::Error> {
    values
        .get(index)
        .and_then(Val::i32)
        .ok_or_else(|| wasmi::Error::new("unexpected import argument"))
}

fn write(caller: &mut Caller<'_, Host>, pointer: i32, bytes: &[u8]) -> Result<(), wasmi::Error> {
    let offset = usize::try_from(pointer).map_err(|_| wasmi::Error::new("negative pointer"))?;
    memory(caller)?
        .write(caller, offset, bytes)
        .map_err(|_| wasmi::Error::new("bounded memory write"))
}

fn host_call(
    mut caller: Caller<'_, Host>,
    name: &str,
    input: &[Val],
    output: &mut [Val],
) -> Result<(), wasmi::Error> {
    let result = match name {
        "a" | "e" => return Err(wasmi::Error::new("native exception or abort")),
        // Native filesystem operations are unavailable, not mapped to host files.
        "d" | "i" | "p" | "n" | "o" => -52,
        "f" => -8,
        "j" => 0,
        "k" => {
            write(&mut caller, integer(input, 0)?, &0_u32.to_le_bytes())?;
            write(&mut caller, integer(input, 1)?, &0_u32.to_le_bytes())?;
            0
        }
        "l" => {
            let clock = integer(input, 0)?;
            if clock != 0 && clock != 1 {
                return Err(wasmi::Error::new("unsupported clock"));
            }
            let duration = if clock == 0 {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|_| wasmi::Error::new("clock before epoch"))?
            } else {
                caller.data().clock.elapsed()
            };
            let ns =
                u64::try_from(duration.as_nanos()).map_err(|_| wasmi::Error::new("clock range"))?;
            write(&mut caller, integer(input, 2)?, &ns.to_le_bytes())?;
            0
        }
        "m" => {
            let wanted = u64::try_from(integer(input, 0)?)
                .map_err(|_| wasmi::Error::new("negative heap size"))?;
            let mem = memory(&caller)?;
            let current = mem.size(&caller);
            let pages = wanted.div_ceil(65536);
            i32::from(
                pages <= current || mem.grow(&mut caller, pages.saturating_sub(current)).is_ok(),
            )
        }
        "b" => {
            if (0..=2).contains(&integer(input, 0)?) {
                0
            } else {
                8
            }
        }
        "g" => 70,
        "h" => {
            if integer(input, 0)? != 0 {
                return Err(wasmi::Error::new("undeclared file read"));
            }
            write(&mut caller, integer(input, 3)?, &0_u32.to_le_bytes())?;
            0
        }
        "c" => {
            let fd = integer(input, 0)?;
            if fd != 1 && fd != 2 {
                return Err(wasmi::Error::new("undeclared file write"));
            }
            let count = usize::try_from(integer(input, 2)?)
                .map_err(|_| wasmi::Error::new("negative vector count"))?;
            if count > 16 {
                return Err(wasmi::Error::new("bounded diagnostic vectors"));
            }
            let address = usize::try_from(integer(input, 1)?)
                .map_err(|_| wasmi::Error::new("negative vector address"))?;
            let mem = memory(&caller)?;
            let mut total = 0_u32;
            for index in 0..count {
                let mut row = [0; 8];
                mem.read(
                    &caller,
                    address
                        .checked_add(index.saturating_mul(8))
                        .ok_or_else(|| wasmi::Error::new("vector address overflow"))?,
                    &mut row,
                )?;
                let pointer = u32::from_le_bytes([row[0], row[1], row[2], row[3]]);
                let length = u32::from_le_bytes([row[4], row[5], row[6], row[7]]);
                total = total
                    .checked_add(length)
                    .ok_or_else(|| wasmi::Error::new("diagnostic length overflow"))?;
                if total > 65536
                    || u64::from(pointer).saturating_add(u64::from(length))
                        > mem.data_size(&caller) as u64
                {
                    return Err(wasmi::Error::new("bounded diagnostic data"));
                }
            }
            // Suppress engine diagnostics; transcriptions use its explicit output buffer.
            write(&mut caller, integer(input, 3)?, &total.to_le_bytes())?;
            0
        }
        _ => return Err(wasmi::Error::new("unlisted publisher import")),
    };
    if let Some(value) = output.first_mut() {
        *value = Val::I32(result);
    }
    Ok(())
}

fn verified(path: &str, size: usize, digest: &str) -> Result<Vec<u8>, Failure> {
    if std::fs::metadata(path)?.len() != size as u64 {
        return Err("artifact size mismatch".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(u64::try_from(size)?.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() != size {
        return Err("artifact changed during read".into());
    }
    if hex::encode(Sha256::digest(&bytes)) != digest {
        return Err("artifact digest mismatch".into());
    }
    Ok(bytes)
}

struct Review {
    store: Store<Host>,
    instance: Instance,
    mem: Memory,
    audio: i32,
    output: i32,
    language: i32,
}

impl Review {
    fn allocate(&mut self, bytes: &[u8]) -> Result<i32, Failure> {
        self.store.set_fuel(FUEL)?;
        let malloc = self.instance.get_typed_func::<i32, i32>(&self.store, "E")?;
        let pointer = malloc.call(&mut self.store, i32::try_from(bytes.len())?)?;
        if pointer <= 0 {
            return Err("bounded allocation failed".into());
        }
        self.mem
            .write(&mut self.store, usize::try_from(pointer)?, bytes)?;
        Ok(pointer)
    }

    fn new(module: &[u8], model: &[u8], language: &str) -> Result<Self, Failure> {
        let mut config = Config::default();
        config.consume_fuel(true);
        config.wasm_simd(true);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, module)?;
        let mut store = Store::new(
            &engine,
            Host {
                limits: StoreLimitsBuilder::new()
                    .memory_size(512 * 1024 * 1024)
                    .build(),
                clock: Instant::now(),
            },
        );
        store.limiter(|host| &mut host.limits);
        store.set_fuel(FUEL)?;
        let mut linker = Linker::new(&engine);
        for import in module.imports() {
            let ExternType::Func(ty) = import.ty() else {
                return Err("unlisted non-function import".into());
            };
            if import.module() != "a"
                || ![
                    "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p",
                ]
                .contains(&import.name())
            {
                return Err("unlisted publisher import".into());
            }
            let name = import.name().to_owned();
            let func = Func::new(&mut store, ty.clone(), move |caller, input, output| {
                host_call(caller, &name, input, output)
            });
            linker.define(import.module(), import.name(), func)?;
        }
        let instance = linker.instantiate_and_start(&mut store, &module)?;
        instance
            .get_typed_func::<(), ()>(&store, "r")?
            .call(&mut store, ())?;
        let mem = instance
            .get_memory(&store, "q")
            .ok_or("declared memory absent")?;
        let mut review = Self {
            store,
            instance,
            mem,
            audio: 0,
            output: 0,
            language: 0,
        };
        let pointer = review.allocate(model)?;
        review.store.set_fuel(FUEL)?;
        let load = instance.get_typed_func::<(i32, i64), i32>(&review.store, "y")?;
        if load.call(&mut review.store, (pointer, i64::try_from(model.len())?))? < 0 {
            return Err("public model load failed".into());
        }
        if instance
            .get_typed_func::<(), i32>(&review.store, "z")?
            .call(&mut review.store, ())?
            & 2
            == 0
        {
            return Err("speech capability absent".into());
        }
        // The public load API does not promise ownership of the passed model.
        // Keep its allocation alive until this isolated store is destroyed.
        review.audio = review.allocate(&vec![0; 30 * 16000 * 4])?;
        review.output = review.allocate(&vec![0; 65536])?;
        review.language = review.allocate(format!("{language}\0").as_bytes())?;
        Ok(review)
    }

    fn pass(&mut self, pcm: &[f32], finish: bool) -> Result<serde_json::Value, Failure> {
        self.store.set_fuel(FUEL)?;
        self.mem.write(
            &mut self.store,
            usize::try_from(self.output)?,
            &vec![0; 65536],
        )?;
        if finish {
            if self
                .instance
                .get_typed_func::<(i32, i32), i32>(&self.store, "w")?
                .call(&mut self.store, (self.output, 65536))?
                < 0
            {
                return Err("public streaming stop failed".into());
            }
        } else {
            let raw: Vec<u8> = pcm.iter().flat_map(|sample| sample.to_le_bytes()).collect();
            self.mem
                .write(&mut self.store, usize::try_from(self.audio)?, &raw)?;
            let process = self
                .instance
                .get_typed_func::<(i32, i32, i32, i32, i32, i32), i32>(&self.store, "v")?;
            if process.call(
                &mut self.store,
                (
                    self.audio,
                    i32::try_from(pcm.len())?,
                    self.language,
                    0,
                    self.output,
                    65536,
                ),
            )? < 0
            {
                return Err("public streaming process failed".into());
            }
        }
        let mut raw = vec![0; 65536];
        self.mem
            .read(&self.store, usize::try_from(self.output)?, &mut raw)?;
        let length = raw
            .iter()
            .position(|byte| *byte == 0)
            .ok_or("bounded native output")?;
        Ok(serde_json::from_slice(
            raw.get(..length).ok_or("output range")?,
        )?)
    }
}

fn main() -> Result<(), Failure> {
    let args: Vec<String> = std::env::args().collect();
    let (module_path, model_path, audio_path, language, sessions) = match args.as_slice() {
        [_, module, model, audio, language] => (module, model, audio, language, 3),
        [_, module, model, audio, language, flag] if flag == "--one-session" => {
            (module, model, audio, language, 1)
        }
        _ => return Err("module model public-f32 language [--one-session] required".into()),
    };
    if !["en", "de", "fr", "es", "it", "nl", "pl"].contains(&language.as_str()) {
        return Err("supported language required".into());
    }
    let module = verified(module_path, 923_348, MODULE_SHA)?;
    let model = verified(model_path, 16_919_407, MODEL_SHA)?;
    let size = std::fs::metadata(audio_path)?.len();
    if size == 0 || size > 30 * 16000 * 4 || size % 4 != 0 {
        return Err("bounded public PCM required".into());
    }
    let mut raw = Vec::new();
    std::fs::File::open(audio_path)?
        .take(size.saturating_add(1))
        .read_to_end(&mut raw)?;
    if raw.len() as u64 != size {
        return Err("public PCM changed during read".into());
    }
    let audio: Vec<f32> = raw
        .chunks_exact(4)
        .map(|row| Ok(f32::from_le_bytes(row.try_into()?)))
        .collect::<Result<_, Failure>>()?;
    if audio
        .iter()
        .any(|sample| !sample.is_finite() || !(-1.0..=1.0).contains(sample))
    {
        return Err("bounded public samples required".into());
    }
    let preparation = Instant::now();
    println!("WHISTLE_WASM_PHASE initialize");
    std::io::stdout().flush()?;
    let mut review = Review::new(&module, &model, language)?;
    println!(
        "WHISTLE_WASM_LOAD {}",
        serde_json::json!({"wall_ms":preparation.elapsed().as_secs_f64()*1000.0,"fuel_per_call":FUEL,"sessions":sessions})
    );
    std::io::stdout().flush()?;
    for session in 0..sessions {
        let silence = vec![0.0; 3 * 16000];
        let pcm = if session == 1 { &silence } else { &audio };
        let start = Instant::now();
        let mut offset = 0_usize;
        let mut planned = 32000_usize;
        while offset < pcm.len() {
            planned = planned.min(pcm.len());
            std::thread::sleep(
                Duration::from_nanos(u64::try_from(planned)?.saturating_mul(62_500))
                    .saturating_sub(start.elapsed()),
            );
            let end = pcm
                .len()
                .min(planned.max(usize::try_from(start.elapsed().as_nanos() / 62_500)?));
            let decoding = Instant::now();
            let result = review.pass(pcm.get(offset..end).ok_or("audio range")?, false)?;
            println!(
                "WHISTLE_WASM_REPLY {}",
                serde_json::json!({"session":session,"kind":"draft","wall_seconds":start.elapsed().as_secs_f64(),"decode_seconds":decoding.elapsed().as_secs_f64(),"result":result})
            );
            std::io::stdout().flush()?;
            offset = end;
            planned = offset.saturating_add(16000);
        }
        let decoding = Instant::now();
        let result = review.pass(&[], true)?;
        println!(
            "WHISTLE_WASM_REPLY {}",
            serde_json::json!({"session":session,"kind":"done","wall_seconds":start.elapsed().as_secs_f64(),"decode_seconds":decoding.elapsed().as_secs_f64(),"result":result})
        );
        std::io::stdout().flush()?;
    }
    Ok(())
}
