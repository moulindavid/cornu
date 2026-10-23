use serde::{Deserialize, Serialize};

use crate::KeyValue;
use crate::key_value;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Environment {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<KeyValue>,
}

impl Environment {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            variables: Vec::new(),
        }
    }

    pub fn with_variable(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.variables.push(KeyValue::new(key, value));
        self
    }

    pub fn enabled(&self) -> impl Iterator<Item = (&str, &str)> {
        key_value::enabled(&self.variables)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Variables;

    #[test]
    fn disabled_variables_are_not_layered() {
        let mut env = Environment::new("dev")
            .with_variable("host", "dev.example")
            .with_variable("token", "t");
        env.variables[1].enabled = false;

        let variables = Variables::new().with_layer(env.enabled());
        assert_eq!(variables.get("host"), Some("dev.example"));
        assert_eq!(variables.get("token"), None);
    }

    #[test]
    fn round_trips_through_toml() {
        let env = Environment::new("prod").with_variable("base_url", "https://api.example");
        let text = toml::to_string(&env).unwrap();
        assert_eq!(toml::from_str::<Environment>(&text).unwrap(), env);
    }
}
