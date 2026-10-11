use std::io::{
   self,
   Write as _,
};

use log::{
   Level,
   LevelFilter,
   Log,
   Metadata,
   Record,
};

const RED: u8 = 31;
const GREEN: u8 = 32;
const YELLOW: u8 = 33;
const BLUE: u8 = 34;

fn paint(color: u8, text: &str) -> String {
   format!("\x1b[{color}m{text}\x1b[0m")
}

struct Logger;

impl Log for Logger {
   fn enabled(&self, _: &Metadata) -> bool {
      true
   }

   fn log(&self, record: &Record) {
      match record.level() {
         Level::Error => {
            eprintln!("{} {}", paint(RED, "Error:"), record.args());
         },
         Level::Warn => {
            eprintln!("{} {}", paint(YELLOW, "Warning:"), record.args());
         },
         Level::Info => {
            eprintln!("{} {}", paint(GREEN, "Info:"), record.args());
         },
         Level::Debug => {
            eprintln!("{} {}", paint(BLUE, "Debug:"), record.args());
         },
         Level::Trace => {
            eprintln!(
               "[{}] {}",
               record.module_path().unwrap_or_default(),
               record.args()
            );
         },
      }
   }

   fn flush(&self) {
      let mut stderr = io::stderr().lock();
      let _ = stderr.flush();
   }
}

pub fn init() {
   log::set_boxed_logger(Box::new(Logger {})).unwrap();
   log::set_max_level(LevelFilter::Info);
}

pub fn enable_debug() {
   log::set_max_level(LevelFilter::Debug);
}
