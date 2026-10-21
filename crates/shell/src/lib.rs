#![no_std]

pub const MAX_ARGUMENTS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    Empty,
    TooManyArguments,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command<'a> {
    pub name: &'a str,
    arguments: [&'a str; MAX_ARGUMENTS],
    argument_count: usize,
}

impl<'a> Command<'a> {
    pub const fn argument_count(&self) -> usize {
        self.argument_count
    }

    pub fn argument(&self, index: usize) -> Option<&'a str> {
        (index < self.argument_count).then_some(self.arguments[index])
    }
}

pub trait ShellIo {
    fn write(&mut self, bytes: &[u8]) -> Result<(), ()>;
    fn cat(&mut self, path: &str) -> Result<(), ()>;
}

pub fn parse(line: &str) -> Result<Command<'_>, ParseError> {
    let mut parts = line.split_ascii_whitespace();
    let Some(name) = parts.next() else {
        return Err(ParseError::Empty);
    };

    let mut arguments = [""; MAX_ARGUMENTS];
    let mut argument_count = 0;
    for part in parts {
        if argument_count == MAX_ARGUMENTS {
            return Err(ParseError::TooManyArguments);
        }
        arguments[argument_count] = part;
        argument_count += 1;
    }

    Ok(Command {
        name,
        arguments,
        argument_count,
    })
}

pub fn execute<I: ShellIo>(command: Command<'_>, io: &mut I) -> i32 {
    match command.name {
        "help" => write_help(io),
        "echo" => echo(command, io),
        "cat" => cat(command, io),
        "true" => 0,
        "false" => 1,
        _ => {
            let _ = io.write(b"phxsh: unknown command: ");
            let _ = io.write(command.name.as_bytes());
            let _ = io.write(b"\n");
            127
        }
    }
}

fn write_help<I: ShellIo>(io: &mut I) -> i32 {
    let text = b"PhoenixOS shell\ncommands: help echo cat true false\n";
    if io.write(text).is_ok() { 0 } else { 1 }
}

fn echo<I: ShellIo>(command: Command<'_>, io: &mut I) -> i32 {
    for index in 0..command.argument_count {
        if index != 0 && io.write(b" ").is_err() {
            return 1;
        }
        let Some(argument) = command.argument(index) else {
            return 1;
        };
        if io.write(argument.as_bytes()).is_err() {
            return 1;
        }
    }
    if io.write(b"\n").is_err() { 1 } else { 0 }
}

fn cat<I: ShellIo>(command: Command<'_>, io: &mut I) -> i32 {
    if command.argument_count != 1 {
        let _ = io.write(b"usage: cat <path>\n");
        return 2;
    }
    let Some(path) = command.argument(0) else {
        return 2;
    };
    if io.cat(path).is_ok() { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestIo {
        output: [u8; 128],
        length: usize,
        cat_path: [u8; 32],
        cat_length: usize,
    }

    impl TestIo {
        const fn new() -> Self {
            Self {
                output: [0; 128],
                length: 0,
                cat_path: [0; 32],
                cat_length: 0,
            }
        }

        fn output(&self) -> &[u8] {
            &self.output[..self.length]
        }
    }

    impl ShellIo for TestIo {
        fn write(&mut self, bytes: &[u8]) -> Result<(), ()> {
            let end = self.length.checked_add(bytes.len()).ok_or(())?;
            if end > self.output.len() {
                return Err(());
            }
            self.output[self.length..end].copy_from_slice(bytes);
            self.length = end;
            Ok(())
        }

        fn cat(&mut self, path: &str) -> Result<(), ()> {
            if path.len() > self.cat_path.len() {
                return Err(());
            }
            self.cat_path[..path.len()].copy_from_slice(path.as_bytes());
            self.cat_length = path.len();
            self.write(b"file-data")
        }
    }

    #[test]
    fn parses_command_without_heap() {
        let command = parse("echo one two").unwrap();
        assert_eq!(command.name, "echo");
        assert_eq!(command.argument_count(), 2);
        assert_eq!(command.argument(0), Some("one"));
        assert_eq!(command.argument(1), Some("two"));
    }

    #[test]
    fn rejects_excessive_argument_count() {
        assert_eq!(
            parse("echo 1 2 3 4 5 6 7 8 9"),
            Err(ParseError::TooManyArguments)
        );
    }

    #[test]
    fn echo_writes_arguments_and_newline() {
        let mut io = TestIo::new();
        assert_eq!(execute(parse("echo Phoenix OS").unwrap(), &mut io), 0);
        assert_eq!(io.output(), b"Phoenix OS\n");
    }

    #[test]
    fn cat_uses_file_adapter() {
        let mut io = TestIo::new();
        assert_eq!(execute(parse("cat /hello.txt").unwrap(), &mut io), 0);
        assert_eq!(&io.cat_path[..io.cat_length], b"/hello.txt");
        assert_eq!(io.output(), b"file-data");
    }

    #[test]
    fn unknown_command_returns_shell_status_127() {
        let mut io = TestIo::new();
        assert_eq!(execute(parse("missing").unwrap(), &mut io), 127);
        assert_eq!(io.output(), b"phxsh: unknown command: missing\n");
    }
}
