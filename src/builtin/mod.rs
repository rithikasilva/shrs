mod cd;
mod exit;
mod history;

use std::process::Child;

use self::{cd::CdBuiltin, exit::ExitBuiltin, history::HistoryBuiltin};
use crate::shell::Context;

pub struct Builtins {
    pub history: Box<dyn BuiltinCmd>,
    pub exit: Box<dyn BuiltinCmd>,
    pub cd: Box<dyn BuiltinCmd>,
}

impl Default for Builtins {
    fn default() -> Self {
        Builtins {
            history: Box::new(HistoryBuiltin::default()),
            exit: Box::new(ExitBuiltin::default()),
            cd: Box::new(CdBuiltin::default()),
        }
    }
}

pub trait BuiltinCmd {
    fn run(&self, ctx: &mut Context, args: &Vec<String>) -> anyhow::Result<Child>;
}