//! The shell's variables: scalars, indexed and associative arrays, their
//! attributes, and `local`'s dynamic scopes.

use std::collections::{BTreeMap, HashMap};

/// A variable's value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Scalar(String),
    /// `a=(x y)`: sparse, by index.
    Indexed(BTreeMap<i64, String>),
    /// `declare -A m`: by key.
    Assoc(BTreeMap<String, String>),
}

impl Value {
    /// The value as a scalar: an array's element 0, as bash reads `$a`.
    pub fn scalar(&self) -> Option<String> {
        match self {
            Self::Scalar(s) => Some(s.clone()),
            Self::Indexed(map) => map.get(&0).cloned(),
            Self::Assoc(map) => map.get("0").cloned(),
        }
    }

    /// Every element, in order.
    pub fn elements(&self) -> Vec<String> {
        match self {
            Self::Scalar(s) => vec![s.clone()],
            Self::Indexed(map) => map.values().cloned().collect(),
            Self::Assoc(map) => map.values().cloned().collect(),
        }
    }

    /// Every index or key, in order.
    pub fn keys(&self) -> Vec<String> {
        match self {
            Self::Scalar(_) => vec!["0".into()],
            Self::Indexed(map) => map.keys().map(ToString::to_string).collect(),
            Self::Assoc(map) => map.keys().cloned().collect(),
        }
    }
}

/// A variable: its value (none when declared but never assigned) and its
/// attributes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Var {
    pub value: Option<Value>,
    pub exported: bool,
    pub readonly: bool,
    /// `declare -i`: assignments are arithmetic.
    pub integer: bool,
}

/// Why an assignment was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOnly(pub String);

impl std::fmt::Display for ReadOnly {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: readonly variable", self.0)
    }
}

/// Every variable, and the scopes `local` opened.
#[derive(Debug, Clone, Default)]
pub struct Vars {
    map: HashMap<String, Var>,
    /// For each function call, what its `local`s shadowed, to put back when
    /// it returns.
    frames: Vec<Vec<(String, Option<Var>)>>,
}

impl Vars {
    pub fn get(&self, name: &str) -> Option<&Var> {
        self.map.get(name)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Var> {
        self.map.get_mut(name)
    }

    /// The scalar value of `name`, if it is set.
    pub fn scalar(&self, name: &str) -> Option<String> {
        self.map
            .get(name)
            .and_then(|v| v.value.as_ref())
            .and_then(Value::scalar)
    }

    pub fn is_set(&self, name: &str) -> bool {
        self.map.get(name).is_some_and(|v| v.value.is_some())
    }

    /// Set `name` to the scalar `value`. An array's element 0 is set instead,
    /// as bash does.
    pub fn set(&mut self, name: &str, value: impl Into<String>) -> Result<(), ReadOnly> {
        let value = value.into();
        let var = self.map.entry(name.to_string()).or_default();
        if var.readonly {
            return Err(ReadOnly(name.to_string()));
        }
        match &mut var.value {
            Some(Value::Indexed(map)) => {
                map.insert(0, value);
            }
            Some(Value::Assoc(map)) => {
                map.insert("0".into(), value);
            }
            other => *other = Some(Value::Scalar(value)),
        }
        Ok(())
    }

    /// Replace the whole value of `name`.
    pub fn set_value(&mut self, name: &str, value: Value) -> Result<(), ReadOnly> {
        let var = self.map.entry(name.to_string()).or_default();
        if var.readonly {
            return Err(ReadOnly(name.to_string()));
        }
        var.value = Some(value);
        Ok(())
    }

    /// Set one element of the array `name`, making it an indexed array if it
    /// was not an array.
    pub fn set_index(&mut self, name: &str, index: i64, value: String) -> Result<(), ReadOnly> {
        let var = self.map.entry(name.to_string()).or_default();
        if var.readonly {
            return Err(ReadOnly(name.to_string()));
        }
        match &mut var.value {
            Some(Value::Indexed(map)) => {
                map.insert(index, value);
            }
            Some(Value::Assoc(map)) => {
                map.insert(index.to_string(), value);
            }
            other => {
                let mut map = BTreeMap::new();
                if let Some(Value::Scalar(old)) = other.take() {
                    map.insert(0, old);
                }
                map.insert(index, value);
                *other = Some(Value::Indexed(map));
            }
        }
        Ok(())
    }

    pub fn set_key(&mut self, name: &str, key: String, value: String) -> Result<(), ReadOnly> {
        let var = self.map.entry(name.to_string()).or_default();
        if var.readonly {
            return Err(ReadOnly(name.to_string()));
        }
        match &mut var.value {
            Some(Value::Assoc(map)) => {
                map.insert(key, value);
            }
            other => {
                let mut map = BTreeMap::new();
                map.insert(key, value);
                *other = Some(Value::Assoc(map));
            }
        }
        Ok(())
    }

    /// Declare `name` without a value (`declare x`, `export x`), keeping one
    /// it has.
    pub fn declare(&mut self, name: &str) -> &mut Var {
        self.map.entry(name.to_string()).or_default()
    }

    pub fn unset(&mut self, name: &str) -> Result<(), ReadOnly> {
        if self.map.get(name).is_some_and(|v| v.readonly) {
            return Err(ReadOnly(name.to_string()));
        }
        self.map.remove(name);
        Ok(())
    }

    pub fn unset_element(&mut self, name: &str, key: &str) -> Result<(), ReadOnly> {
        let Some(var) = self.map.get_mut(name) else {
            return Ok(());
        };
        if var.readonly {
            return Err(ReadOnly(name.to_string()));
        }
        match &mut var.value {
            Some(Value::Indexed(map)) => {
                if let Ok(index) = key.parse::<i64>() {
                    map.remove(&index);
                }
            }
            Some(Value::Assoc(map)) => {
                map.remove(key);
            }
            Some(Value::Scalar(_)) if key == "0" => var.value = None,
            _ => {}
        }
        Ok(())
    }

    /// Open a function's scope.
    pub fn push_frame(&mut self) {
        self.frames.push(Vec::new());
    }

    /// Close it: every `local` gets back what it shadowed.
    pub fn pop_frame(&mut self) {
        let Some(frame) = self.frames.pop() else {
            return;
        };
        for (name, old) in frame.into_iter().rev() {
            match old {
                Some(var) => {
                    self.map.insert(name, var);
                }
                None => {
                    self.map.remove(&name);
                }
            }
        }
    }

    pub fn in_function(&self) -> bool {
        !self.frames.is_empty()
    }

    /// Make `name` local to the innermost function. It starts unset, unless
    /// the caller's was exported, which it stays.
    pub fn make_local(&mut self, name: &str) {
        let Some(frame) = self.frames.last_mut() else {
            return;
        };
        if frame.iter().any(|(n, _)| n == name) {
            return;
        }
        let old = self.map.get(name).cloned();
        frame.push((name.to_string(), old.clone()));
        let exported = old.as_ref().is_some_and(|v| v.exported);
        self.map.insert(
            name.to_string(),
            Var {
                value: None,
                exported,
                ..Var::default()
            },
        );
    }

    /// The environment a program gets: every exported variable with a value.
    pub fn exported(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .map
            .iter()
            .filter(|(_, v)| v.exported)
            .filter_map(|(k, v)| Some((k.clone(), v.value.as_ref()?.scalar()?)))
            .collect();
        out.sort();
        out
    }

    /// Every variable, sorted by name.
    pub fn all(&self) -> Vec<(&String, &Var)> {
        let mut out: Vec<_> = self.map.iter().collect();
        out.sort_by(|a, b| a.0.cmp(b.0));
        out
    }
}

/// Whether `name` is a valid variable name.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}
