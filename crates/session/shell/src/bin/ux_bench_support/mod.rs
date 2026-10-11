//! Bounded visible-input JSONL transport. Coordinates and scroll deltas are
//! logical pixels; positive wheel dy moves toward content below the viewport.
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};
use std::path::Path;
use ui::headless::FrameHarness;
use ui::ux_bench::Counts;
use winit::{
    dpi::PhysicalPosition,
    event::{DeviceId, ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent},
};

#[cfg(test)]
mod tests;

const MAX_LINE: u64 = 32_768;
const MAX_COMMANDS: u32 = 512;

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    Observe {},
    Click {
        x: f64,
        y: f64,
    },
    /// A secondary (right) button press and release at a logical point:
    /// the pointer route to a context menu, which no semantic action opens.
    ContextClick {
        x: f64,
        y: f64,
    },
    Key {
        key: String,
        modifiers: Vec<String>,
    },
    Type {
        text: String,
    },
    Wheel {
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
    },
    Finish {},
}

fn validate_command(raw: &str, viewport: [u32; 2]) -> Result<Command, String> {
    let command: Command =
        serde_json::from_str(raw).map_err(|e| format!("invalid command: {e}"))?;
    match &command {
        Command::Click { x, y } | Command::ContextClick { x, y } | Command::Wheel { x, y, .. } => {
            if !x.is_finite()
                || !y.is_finite()
                || *x < 0.
                || *y < 0.
                || *x >= f64::from(viewport[0])
                || *y >= f64::from(viewport[1])
            {
                return Err("coordinates must be finite and inside the logical viewport".into());
            }
        }
        Command::Key { key, modifiers } => {
            if !matches!(
                key.as_str(),
                "Enter"
                    | "Return"
                    | "Tab"
                    | "Escape"
                    | "Esc"
                    | "Space"
                    | "Backspace"
                    | "Delete"
                    | "ArrowUp"
                    | "Up"
                    | "ArrowDown"
                    | "Down"
                    | "ArrowLeft"
                    | "Left"
                    | "ArrowRight"
                    | "Right"
                    | "Home"
                    | "End"
            ) && !(key.chars().count() == 1 && !key.chars().any(char::is_control))
            {
                return Err("key must be one printable character or a supported named key".into());
            }
            if modifiers.len() > 4
                || modifiers.iter().any(|m| {
                    !matches!(
                        m.as_str(),
                        "ctrl" | "control" | "shift" | "alt" | "logo" | "super" | "meta"
                    )
                })
            {
                return Err("modifiers must contain at most four of ctrl/control, shift, alt, logo/super/meta".into());
            }
        }
        Command::Type { text } => {
            if text.len() > 4096 || text.chars().any(char::is_control) {
                return Err("text must be at most 4096 UTF-8 bytes without control characters; use key for Enter/Tab".into());
            }
        }
        _ => {}
    }
    if let Command::Wheel { dx, dy, .. } = &command {
        if !dx.is_finite() || !dy.is_finite() || dx.abs() > 10_000. || dy.abs() > 10_000. {
            return Err(
                "wheel deltas must be finite logical pixels with magnitude at most 10000".into(),
            );
        }
    }
    Ok(command)
}

fn apply(driver: &mut FrameHarness, command: &Command, scale: f64, counts: &mut Counts) {
    match command {
        Command::Click { x, y } => {
            driver.click_at(*x, *y);
            counts.clicks += 1;
        }
        Command::ContextClick { x, y } => {
            // Through the production dispatch path (`FrameHarness::send`), like
            // `click_at` does for the primary button: move, press, release.
            let device_id = DeviceId::dummy();
            driver.send(&WindowEvent::CursorMoved {
                device_id,
                position: PhysicalPosition::new(x * scale, y * scale),
            });
            for state in [ElementState::Pressed, ElementState::Released] {
                driver.send(&WindowEvent::MouseInput {
                    device_id,
                    state,
                    button: MouseButton::Right,
                });
            }
            counts.clicks += 1;
        }
        Command::Key { key, modifiers } => {
            driver.set_modifiers(modifiers);
            driver.key(key);
            driver.set_modifiers(&[]);
            counts.keys += 1;
        }
        Command::Type { text } => {
            driver.type_text(text);
            counts.keys += u32::try_from(text.chars().count()).unwrap_or(4096);
        }
        Command::Wheel { x, y, dx, dy } => {
            driver.send(&WindowEvent::CursorMoved {
                device_id: DeviceId::dummy(),
                position: PhysicalPosition::new(x * scale, y * scale),
            });
            driver.send(&WindowEvent::MouseWheel {
                device_id: DeviceId::dummy(),
                delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(
                    -dx * scale,
                    -dy * scale,
                )),
                phase: TouchPhase::Moved,
            });
            counts.wheel_events += 1;
        }
        Command::Observe {} | Command::Finish {} => {}
    }
}

fn capture(
    driver: &mut FrameHarness,
    out: &Path,
    sequence: u32,
    counts: &Counts,
    state: &mut impl FnMut() -> Value,
    error: Option<String>,
) -> Result<Value, String> {
    driver.relayout();
    let screenshot = out.join(format!("{sequence:04}.png"));
    let inspection = out.join(format!("{sequence:04}.inspection.json"));
    driver.render_png(&screenshot);
    if !screenshot.is_file() {
        return Err("renderer did not produce screenshot".into());
    }
    std::fs::write(
        &inspection,
        driver.inspection_json().map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(
        json!({"ok":error.is_none(),"sequence":sequence,"screenshot":screenshot,"inspection":inspection,"state":state(),"counts":counts,"error":error}),
    )
}

fn emit(output: &mut impl Write, value: &Value) -> Result<(), String> {
    serde_json::to_writer(&mut *output, value).map_err(|e| e.to_string())?;
    writeln!(output).map_err(|e| e.to_string())?;
    output.flush().map_err(|e| e.to_string())
}

pub(super) fn run(
    driver: &mut FrameHarness,
    scenario: &ui::ux_bench::Scenario,
    out: &Path,
    mut state: impl FnMut() -> Value,
    mut finalize: impl FnMut() -> Result<Vec<String>, String>,
) -> Result<bool, String> {
    driver.set_input_scale(scenario.scale)?;
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    let mut counts = Counts::default();
    emit(
        &mut output,
        &capture(driver, out, 0, &counts, &mut state, None)?,
    )?;
    for sequence in 1..=MAX_COMMANDS + 1 {
        let mut line = Vec::new();
        let bytes = input
            .by_ref()
            .take(MAX_LINE + 1)
            .read_until(b'\n', &mut line)
            .map_err(|e| e.to_string())?;
        if bytes == 0 {
            finalize()?;
            return Err("stdin closed without finish; state saved".into());
        }
        let fatal = bytes as u64 > MAX_LINE || sequence > MAX_COMMANDS;
        let command = if bytes as u64 > MAX_LINE {
            Err("command exceeds 32768 bytes; session terminated".into())
        } else if sequence > MAX_COMMANDS {
            Err("command limit of 512 reached; session terminated".into())
        } else {
            std::str::from_utf8(&line)
                .map_err(|_| "command must be UTF-8 JSON".to_string())
                .and_then(|raw| validate_command(raw, scenario.viewport))
        };
        let finish = matches!(command, Ok(Command::Finish {}));
        let mut error = command.as_ref().err().cloned();
        if let Ok(command) = &command {
            apply(driver, command, scenario.scale, &mut counts);
        }
        let persisted = if finish || fatal {
            driver.relayout();
            match finalize() {
                Ok(names) => Some(names),
                Err(e) => {
                    error = Some(e);
                    None
                }
            }
        } else {
            None
        };
        let mut response = capture(driver, out, sequence, &counts, &mut state, error)?;
        if finish {
            response["persisted_workspace_names"] = json!(persisted.unwrap_or_default());
        }
        emit(&mut output, &response)?;
        if finish || fatal {
            return Ok(response["ok"] == true);
        }
    }
    Err("command limit exhausted".into())
}
