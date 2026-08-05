use assert_cmd::Command;

pub fn command() -> Command {
    let mut command = Command::cargo_bin("stalelink").unwrap();
    isolate(&mut command);
    command
}

#[allow(dead_code)] // Each integration test crate compiles this module independently.
pub fn child_command() -> std::process::Command {
    let binary = Command::cargo_bin("stalelink").unwrap();
    let mut command = std::process::Command::new(binary.get_program());
    isolate(&mut command);
    command
}

trait IsolatedCommand {
    fn remove_env(&mut self, name: &str);
}

impl IsolatedCommand for Command {
    fn remove_env(&mut self, name: &str) {
        self.env_remove(name);
    }
}

impl IsolatedCommand for std::process::Command {
    fn remove_env(&mut self, name: &str) {
        self.env_remove(name);
    }
}

fn isolate(command: &mut impl IsolatedCommand) {
    for (name, _) in
        std::env::vars_os().filter(|(name, _)| name.to_string_lossy().starts_with("STALELINK_"))
    {
        command.remove_env(&name.to_string_lossy());
    }
}
