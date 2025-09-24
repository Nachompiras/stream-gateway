use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub auto_port_range_start: u16,
    pub auto_port_range_end: u16,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            auto_port_range_start: env::var("AUTO_PORT_RANGE_START")
                .unwrap_or_else(|_| "10000".to_string())
                .parse()
                .unwrap_or(10000),
            auto_port_range_end: env::var("AUTO_PORT_RANGE_END")
                .unwrap_or_else(|_| "11000".to_string())
                .parse()
                .unwrap_or(11000),
        }
    }

    pub fn port_range(&self) -> std::ops::Range<u16> {
        self.auto_port_range_start..self.auto_port_range_end
    }
}

pub static CONFIG: once_cell::sync::Lazy<Config> = once_cell::sync::Lazy::new(Config::from_env);