use std::collections::HashMap;
use std::fmt;

pub struct ArgsBuilder(HashMap<String, Vec<String>>);

impl ArgsBuilder {
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    pub fn has(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    pub fn arg<T: Into<Arg>>(&mut self, arg: T) -> &mut Self {
        let arg = arg.into();
        if let Some(values) = self.0.get_mut(&arg.key) {
            values.extend(arg.values);
        } else {
            self.0.insert(arg.key, arg.values);
        }
        self
    }

    pub fn args<T: Into<Arg>>(&mut self, args: impl IntoIterator<Item = T>) -> &mut Self {
        for arg in args {
            self.arg(arg);
        }
        self
    }

    pub fn into_iter(self) -> impl Iterator<Item = String> {
        self.0.into_iter().map(|(key, values)| {
            if values.is_empty() {
                format!("--{}", key)
            } else {
                format!("--{}={}", key, values.join(","))
            }
        })
    }
}

#[derive(Debug, Clone)]
pub struct Arg {
    key: String,
    values: Vec<String>,
}

impl Arg {
    pub fn key(key: impl AsRef<str>) -> Self {
        Self {
            key: key.as_ref().to_string(),
            values: Vec::new(),
        }
    }

    pub fn value(key: impl AsRef<str>, value: impl fmt::Display) -> Self {
        Self {
            key: key.as_ref().to_string(),
            values: vec![value.to_string()],
        }
    }

    pub fn values(
        key: impl AsRef<str>,
        values: impl IntoIterator<Item = impl fmt::Display>,
    ) -> Self {
        Self {
            key: key.as_ref().to_string(),
            values: values.into_iter().map(|v| v.to_string()).collect(),
        }
    }
}

impl From<(&str, &str)> for Arg {
    fn from((key, value): (&str, &str)) -> Self {
        Self {
            key: key.to_string(),
            values: vec![value.to_string()],
        }
    }
}

impl From<(&str, &[&str])> for Arg {
    fn from((key, values): (&str, &[&str])) -> Self {
        Self {
            key: key.to_string(),
            values: values.iter().map(|v| v.to_string()).collect(),
        }
    }
}

impl From<&str> for Arg {
    fn from(value: &str) -> Self {
        let value = value.strip_prefix("--").unwrap_or(value);
        match value.split_once('=') {
            Some((key, val)) => Self {
                key: key.to_string(),
                values: vec![val.to_string()],
            },
            None => Self {
                key: value.to_string(),
                values: Vec::new(),
            },
        }
    }
}

impl From<String> for Arg {
    fn from(value: String) -> Self {
        Arg::from(value.as_str())
    }
}

impl From<ArgConst> for Arg {
    fn from(arg: ArgConst) -> Self {
        Self {
            key: arg.key.to_string(),
            values: arg.values.iter().map(|v| v.to_string()).collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ArgConst {
    key: &'static str,
    values: &'static [&'static str],
}

impl ArgConst {
    pub const fn key(key: &'static str) -> Self {
        Self { key, values: &[] }
    }

    pub const fn values(key: &'static str, values: &'static [&'static str]) -> Self {
        Self { key, values }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arg_from_str_parses_key_value() {
        let mut builder = ArgsBuilder::new();
        builder.arg("--kiosk=about:blank");
        let result = builder.into_iter().next().unwrap();
        assert_eq!(result, "--kiosk=about:blank");
    }

    #[test]
    fn arg_from_str_merges_with_existing() {
        let mut builder = ArgsBuilder::new();
        builder.arg("--disable-features=Foo");
        builder.arg(Arg::value("disable-features", "Bar"));
        let result = builder.into_iter().next().unwrap();
        assert_eq!(result, "--disable-features=Foo,Bar");
    }
}
