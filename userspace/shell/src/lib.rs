#![no_std]

use indo_syscall as sys;

// ═══════════════════════════════════════════════════════════════════════════════
// Constants
// ═══════════════════════════════════════════════════════════════════════════════

const MAX_INPUT: usize = 512;
const MAX_ARGS: usize = 32;
const MAX_PATH: usize = 256;
const MAX_CWD: usize = 256;
const MAX_TOKENS: usize = 64;
const MAX_CMDS: usize = 8;

// ═══════════════════════════════════════════════════════════════════════════════
// Shell state
// ═══════════════════════════════════════════════════════════════════════════════

const MAX_JOBS: usize = 16;
const MAX_CMD_NAME: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobState {
    Running,
    Stopped,
    Done,
}

#[derive(Clone, Copy)]
struct Job {
    pgid: u64,
    state: JobState,
    cmd_name: [u8; MAX_CMD_NAME],
}

impl Job {
    const fn new() -> Self {
        Job {
            pgid: 0,
            state: JobState::Done,
            cmd_name: [0u8; MAX_CMD_NAME],
        }
    }

    fn set_name(&mut self, name: &str) {
        self.cmd_name = [0u8; MAX_CMD_NAME];
        let bytes = name.as_bytes();
        let len = core::cmp::min(bytes.len(), MAX_CMD_NAME - 1);
        self.cmd_name[..len].copy_from_slice(&bytes[..len]);
    }

    fn name_str(&self) -> &str {
        let len = self.cmd_name.iter().position(|&b| b == 0).unwrap_or(MAX_CMD_NAME);
        core::str::from_utf8(&self.cmd_name[..len]).unwrap_or("")
    }
}

static mut JOBS: [Job; MAX_JOBS] = {
    const INIT: Job = Job::new();
    [INIT; MAX_JOBS]
};

static mut CURRENT_FG: usize = 0; // index into JOBS, 0 = no foreground job
static mut LAST_EXIT_CODE: i64 = 0; // $?

fn find_free_job_slot() -> Option<usize> {
    unsafe {
        for i in 0..MAX_JOBS {
            if JOBS[i].state == JobState::Done {
                return Some(i);
            }
        }
    }
    None
}

fn add_job(pgid: u64, name: &str) -> Option<usize> {
    unsafe {
        if let Some(slot) = find_free_job_slot() {
            JOBS[slot].pgid = pgid;
            JOBS[slot].state = JobState::Running;
            JOBS[slot].set_name(name);
            Some(slot)
        } else {
            None
        }
    }
}

fn cleanup_done_jobs() {
    unsafe {
        for i in 0..MAX_JOBS {
            if JOBS[i].state == JobState::Done {
                JOBS[i].pgid = 0;
            }
        }
    }
}

static mut CWD: [u8; MAX_CWD] = {
    let mut arr = [0u8; MAX_CWD];
    arr[0] = b'/';
    arr
};

fn cwd_str() -> &'static str {
    unsafe {
        let len = CWD.iter().position(|&b| b == 0).unwrap_or(MAX_CWD);
        core::str::from_utf8(&CWD[..len]).unwrap_or("/")
    }
}

fn set_cwd(path: &str) {
    unsafe {
        CWD = [0u8; MAX_CWD];
        let bytes = path.as_bytes();
        let copy_len = core::cmp::min(bytes.len(), MAX_CWD - 1);
        CWD[..copy_len].copy_from_slice(&bytes[..copy_len]);
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tokenizer
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenKind {
    Word,
    Pipe,
    RedirectOut,
    AppendOut,
    RedirectIn,
}

#[derive(Copy, Clone)]
struct Token {
    start: usize,
    end: usize,
    kind: TokenKind,
}

fn tokenize(input: &[u8], tokens: &mut [Token]) -> usize {
    let mut count = 0;
    let mut i = 0;
    let len = input.len();

    while i < len && count < tokens.len() {
        while i < len && (input[i] == b' ' || input[i] == b'\t') {
            i += 1;
        }
        if i >= len {
            break;
        }

        match input[i] {
            b'|' => {
                tokens[count] = Token { start: i, end: i + 1, kind: TokenKind::Pipe };
                i += 1;
                count += 1;
            }
            b'>' => {
                if i + 1 < len && input[i + 1] == b'>' {
                    tokens[count] = Token { start: i, end: i + 2, kind: TokenKind::AppendOut };
                    i += 2;
                } else {
                    tokens[count] = Token { start: i, end: i + 1, kind: TokenKind::RedirectOut };
                    i += 1;
                }
                count += 1;
            }
            b'<' => {
                tokens[count] = Token { start: i, end: i + 1, kind: TokenKind::RedirectIn };
                i += 1;
                count += 1;
            }
            b'"' => {
                i += 1;
                let start = i;
                while i < len && input[i] != b'"' {
                    i += 1;
                }
                tokens[count] = Token { start, end: i, kind: TokenKind::Word };
                if i < len { i += 1; }
                count += 1;
            }
            _ => {
                let start = i;
                while i < len
                    && input[i] != b' '
                    && input[i] != b'\t'
                    && input[i] != b'|'
                    && input[i] != b'>'
                    && input[i] != b'<'
                {
                    i += 1;
                }
                tokens[count] = Token { start, end: i, kind: TokenKind::Word };
                count += 1;
            }
        }
    }
    count
}

// ═══════════════════════════════════════════════════════════════════════════════
// Command representation
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Copy, Clone)]
struct ParsedCmd {
    input_line: [u8; MAX_INPUT],
    input_len: usize,
    arg_offsets: [(usize, usize); MAX_ARGS],
    arg_count: usize,
    stdin_file: [u8; MAX_PATH],
    stdin_len: usize,
    stdout_file: [u8; MAX_PATH],
    stdout_len: usize,
    append_mode: bool,
}

impl ParsedCmd {
    fn new() -> Self {
        ParsedCmd {
            input_line: [0u8; MAX_INPUT],
            input_len: 0,
            arg_offsets: [(0, 0); MAX_ARGS],
            arg_count: 0,
            stdin_file: [0u8; MAX_PATH],
            stdin_len: 0,
            stdout_file: [0u8; MAX_PATH],
            stdout_len: 0,
            append_mode: false,
        }
    }

    fn arg(&self, idx: usize) -> &[u8] {
        if idx >= self.arg_count { return b""; }
        let (s, e) = self.arg_offsets[idx];
        &self.input_line[s..e]
    }

    fn stdin_str(&self) -> &str {
        if self.stdin_len == 0 { return ""; }
        core::str::from_utf8(&self.stdin_file[..self.stdin_len]).unwrap_or("")
    }

    fn stdout_str(&self) -> &str {
        if self.stdout_len == 0 { return ""; }
        core::str::from_utf8(&self.stdout_file[..self.stdout_len]).unwrap_or("")
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Parse tokens into commands
// ═══════════════════════════════════════════════════════════════════════════════

fn parse_pipeline(
    input: &[u8],
    tokens: &[Token],
    token_count: usize,
    cmds: &mut [ParsedCmd],
) -> usize {
    let mut cmd_count = 0;
    let mut arg_idx = 0;
    let mut current = ParsedCmd::new();
    let input_len = input.len();
    let copy_len = core::cmp::min(input_len, MAX_INPUT);
    current.input_line[..copy_len].copy_from_slice(&input[..copy_len]);
    current.input_len = copy_len;

    for i in 0..token_count {
        match tokens[i].kind {
            TokenKind::Word => {
                if arg_idx < MAX_ARGS {
                    let s = tokens[i].start;
                    let e = core::cmp::min(tokens[i].end, input_len);
                    current.arg_offsets[arg_idx] = (s, e);
                    arg_idx += 1;
                    current.arg_count = arg_idx;
                }
            }
            TokenKind::RedirectIn => {
                let s = tokens[i].start;
                let e = core::cmp::min(tokens[i].end, input_len);
                let len = core::cmp::min(e - s, MAX_PATH - 1);
                current.stdin_len = len;
                current.stdin_file[..len].copy_from_slice(&input[s..s + len]);
            }
            TokenKind::RedirectOut => {
                let s = tokens[i].start;
                let e = core::cmp::min(tokens[i].end, input_len);
                let len = core::cmp::min(e - s, MAX_PATH - 1);
                current.stdout_len = len;
                current.stdout_file[..len].copy_from_slice(&input[s..s + len]);
                current.append_mode = false;
            }
            TokenKind::AppendOut => {
                let s = tokens[i].start;
                let e = core::cmp::min(tokens[i].end, input_len);
                let len = core::cmp::min(e - s, MAX_PATH - 1);
                current.stdout_len = len;
                current.stdout_file[..len].copy_from_slice(&input[s..s + len]);
                current.append_mode = true;
            }
            TokenKind::Pipe => {
                if cmd_count < cmds.len() {
                    cmds[cmd_count] = current;
                    cmd_count += 1;
                }
                current = ParsedCmd::new();
                let copy_len = core::cmp::min(input_len, MAX_INPUT);
                current.input_line[..copy_len].copy_from_slice(&input[..copy_len]);
                current.input_len = copy_len;
                arg_idx = 0;
            }
        }
    }

    if arg_idx > 0 || current.arg_count > 0 {
        if cmd_count < cmds.len() {
            cmds[cmd_count] = current;
            cmd_count += 1;
        }
    }

    cmd_count
}

// ═══════════════════════════════════════════════════════════════════════════════
// Utility functions
// ═══════════════════════════════════════════════════════════════════════════════

fn write_str(s: &str) {
    sys::write(1, s.as_bytes());
}

fn write_str_num(n: u64) {
    if n == 0 {
        sys::write(1, b"0");
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    let mut val = n;
    while val > 0 {
        i -= 1;
        buf[i] = b'0' + (val % 10) as u8;
        val /= 10;
    }
    sys::write(1, &buf[i..]);
}

fn write_i64(n: i64) {
    if n < 0 {
        sys::write(1, b"-");
        write_str_num((-(n as i128)) as u64);
    } else {
        write_str_num(n as u64);
    }
}

fn write_hex(val: u64) {
    sys::write(1, b"0x");
    let mut buf = [0u8; 16];
    let hex = b"0123456789abcdef";
    let mut i = 0;
    let mut v = val;
    if v == 0 {
        buf[0] = b'0';
        i = 1;
    } else {
        while v > 0 {
            buf[i] = hex[(v & 0xF) as usize];
            v >>= 4;
            i += 1;
        }
        buf[..i].reverse();
    }
    sys::write(1, &buf[..i]);
}

fn parse_decimal(bytes: &[u8]) -> u64 {
    let mut n: u64 = 0;
    for &b in bytes {
        if b >= b'0' && b <= b'9' {
            n = n * 10 + (b - b'0') as u64;
        } else {
            break;
        }
    }
    n
}

fn write_err(cmd: &str, msg: &str) {
    sys::write(2, b"indosh: ");
    sys::write(2, cmd.as_bytes());
    sys::write(2, b": ");
    sys::write(2, msg.as_bytes());
    sys::write(2, b"\n");
}

fn print_decimal(mut n: u64) {
    if n == 0 {
        sys::write(1, b"0");
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    sys::write(1, &buf[i..]);
}

fn bytes_eq(a: &[u8], b: &str) -> bool {
    a == b.as_bytes()
}

fn to_str(b: &[u8]) -> Option<&str> {
    core::str::from_utf8(b).ok()
}

fn resolve_path(cmd: &[u8], buf: &mut [u8; MAX_PATH]) -> bool {
    if cmd.is_empty() { return false; }
    if cmd[0] == b'/' {
        let len = core::cmp::min(cmd.len(), MAX_PATH - 1);
        buf[..len].copy_from_slice(&cmd[..len]);
        buf[len] = 0;
        return true;
    }
    let prefix = b"/bin/";
    let total = prefix.len() + cmd.len();
    if total >= MAX_PATH { return false; }
    buf[..prefix.len()].copy_from_slice(prefix);
    buf[prefix.len()..prefix.len() + cmd.len()].copy_from_slice(cmd);
    buf[prefix.len() + cmd.len()] = 0;
    true
}

fn is_executable(path: &str) -> bool {
    let fd = sys::open(path, sys::O_RDONLY);
    if sys::is_error(fd) { return false; }
    let mut header = [0u8; 4];
    let n = sys::read(fd as u64, &mut header);
    sys::close(fd as u64);
    if sys::is_error(n) || (n as usize) < 4 { return false; }
    header == [0x7f, b'E', b'L', b'F']
}

fn normalize_path(input: &[u8], output: &mut [u8]) -> usize {
    let mut parts: [(usize, usize); 64] = [(0, 0); 64];
    let mut part_count = 0;
    let mut i = 0;
    let len = input.len();

    while i < len && input[i] == b'/' { i += 1; }

    while i < len && part_count < parts.len() {
        let start = i;
        while i < len && input[i] != b'/' { i += 1; }
        let comp = &input[start..i];
        if comp == b"." || comp.is_empty() {
            // skip
        } else if comp == b".." {
            if part_count > 0 { part_count -= 1; }
        } else {
            parts[part_count] = (start, i - start);
            part_count += 1;
        }
        while i < len && input[i] == b'/' { i += 1; }
    }

    let mut pos = 0;
    output[pos] = b'/';
    pos += 1;
    for p in 0..part_count {
        let (start, slen) = parts[p];
        if p > 0 {
            output[pos] = b'/';
            pos += 1;
        }
        let copy_len = core::cmp::min(slen, output.len() - pos - 1);
        output[pos..pos + copy_len].copy_from_slice(&input[start..start + copy_len]);
        pos += copy_len;
    }

    output[pos] = 0;
    pos
}

fn resolve_full_path(path: &[u8], output: &mut [u8]) -> bool {
    if path.is_empty() { return false; }
    if path[0] == b'/' {
        let len = normalize_path(path, output);
        output[len] = 0;
        return true;
    }
    let cwd = unsafe { CWD };
    let cwd_len = cwd.iter().position(|&b| b == 0).unwrap_or(MAX_CWD);
    let total = cwd_len + 1 + path.len();
    if total >= output.len() { return false; }
    let mut tmp = [0u8; MAX_PATH];
    let mut pos = 0;
    tmp[pos..pos + cwd_len].copy_from_slice(&cwd[..cwd_len]);
    pos += cwd_len;
    if cwd_len > 0 && cwd[cwd_len - 1] != b'/' {
        tmp[pos] = b'/';
        pos += 1;
    }
    let path_len = core::cmp::min(path.len(), tmp.len() - pos);
    tmp[pos..pos + path_len].copy_from_slice(&path[..path_len]);
    pos += path_len;

    let len = normalize_path(&tmp[..pos], output);
    output[len] = 0;
    true
}

// ═══════════════════════════════════════════════════════════════════════════════
// Built-in commands
// ═══════════════════════════════════════════════════════════════════════════════

fn is_builtin(cmd: &[u8]) -> bool {
    bytes_eq(cmd, "help") || bytes_eq(cmd, "exit") || bytes_eq(cmd, "echo")
        || bytes_eq(cmd, "pwd") || bytes_eq(cmd, "cd") || bytes_eq(cmd, "clear")
        || bytes_eq(cmd, "cat") || bytes_eq(cmd, "ls") || bytes_eq(cmd, "mkdir")
        || bytes_eq(cmd, "touch") || bytes_eq(cmd, "rm") || bytes_eq(cmd, "pid")
        || bytes_eq(cmd, "ps") || bytes_eq(cmd, "true") || bytes_eq(cmd, "false")
        || bytes_eq(cmd, "jobs") || bytes_eq(cmd, "fg") || bytes_eq(cmd, "bg")
        || bytes_eq(cmd, "status")
}

fn run_builtin(cmd: &ParsedCmd) -> i64 {
    let name = cmd.arg(0);

    if bytes_eq(name, "help") {
        write_str("Commands:\n");
        write_str("  help              - show this help\n");
        write_str("  exit              - exit shell\n");
        write_str("  echo [text...]    - print text\n");
        write_str("  pwd               - print working directory\n");
        write_str("  cd <path>         - change directory\n");
        write_str("  clear             - clear screen\n");
        write_str("  cat <file>        - display file contents\n");
        write_str("  ls [dir]          - list directory\n");
        write_str("  mkdir <dir>       - create directory\n");
        write_str("  touch <file>      - create empty file\n");
        write_str("  rm <file>         - delete file\n");
        write_str("  pid               - show current PID\n");
        write_str("  exec <file> [args]- execute program\n");
        write_str("  true / false      - exit status\n");
        write_str("  jobs              - list background jobs\n");
        write_str("  fg [job]          - bring job to foreground\n");
        write_str("  bg [job]          - resume job in background\n");
        write_str("  status            - show last exit code ($?)\n");
        0
    }
    else if bytes_eq(name, "exit") {
        sys::exit(0);
    }
    else if bytes_eq(name, "echo") {
        for i in 1..cmd.arg_count {
            if i > 1 { sys::write(1, b" "); }
            sys::write(1, cmd.arg(i));
        }
        sys::write(1, b"\n");
        0
    }
    else if bytes_eq(name, "pwd") {
        write_str(cwd_str());
        sys::write(1, b"\n");
        0
    }
    else if bytes_eq(name, "cd") {
        if cmd.arg_count < 2 {
            set_cwd("/");
            return 0;
        }
        let path = cmd.arg(1);
        let mut resolved = [0u8; MAX_PATH];
        if !resolve_full_path(path, &mut resolved) {
            write_err("cd", "path too long");
            return 1;
        }
        let path_str = match to_str(&resolved) {
            Some(s) => s,
            None => { write_err("cd", "invalid path"); return 1; }
        };
        let ret = sys::chdir(path_str);
        if sys::is_error(ret) {
            write_err("cd", "no such directory");
            return 1;
        }
        set_cwd(path_str);
        0
    }
    else if bytes_eq(name, "clear") {
        sys::write(1, b"\x1b[2J\x1b[H");
        0
    }
    else if bytes_eq(name, "cat") {
        if cmd.arg_count < 2 { write_err("cat", "missing operand"); return 1; }
        let path = cmd.arg(1);
        let mut resolved = [0u8; MAX_PATH];
        if !resolve_full_path(path, &mut resolved) { write_err("cat", "path too long"); return 1; }
        let path_str = match to_str(&resolved) {
            Some(s) => s, None => { write_err("cat", "invalid path"); return 1; }
        };
        let fd = sys::open(path_str, sys::O_RDONLY);
        if sys::is_error(fd) { write_err("cat", "file not found"); return 1; }
        let mut buf = [0u8; 4096];
        loop {
            let n = sys::read(fd as u64, &mut buf);
            if sys::is_error(n) || n == 0 { break; }
            sys::write(1, &buf[..n as usize]);
        }
        sys::close(fd as u64);
        0
    }
    else if bytes_eq(name, "ls") {
        let dir_path = if cmd.arg_count > 1 { cmd.arg(1) } else { b"/" };
        let mut resolved = [0u8; MAX_PATH];
        if !resolve_full_path(dir_path, &mut resolved) { write_err("ls", "path too long"); return 1; }
        let path_str = match to_str(&resolved) {
            Some(s) => s, None => { write_err("ls", "invalid path"); return 1; }
        };
        let fd = sys::open(path_str, sys::O_RDONLY);
        if sys::is_error(fd) { write_err("ls", "directory not found"); return 1; }
        let mut buf = [0u8; 512];
        let mut first = true;
        loop {
            let n = sys::readdir(fd as u64, &mut buf);
            if sys::is_error(n) || n == 0 { break; }
            let data = &buf[..n as usize];
            let mut i = 0;
            while i < data.len() {
                let name_len = data[i] as usize;
                if name_len == 0 || i + 1 + name_len > data.len() { break; }
                if !first { sys::write(1, b"  "); }
                first = false;
                sys::write(1, &data[i + 1..i + 1 + name_len]);
                i += 1 + name_len;
            }
        }
        sys::write(1, b"\n");
        sys::close(fd as u64);
        0
    }
    else if bytes_eq(name, "mkdir") {
        if cmd.arg_count < 2 { write_err("mkdir", "missing operand"); return 1; }
        let path = cmd.arg(1);
        let mut resolved = [0u8; MAX_PATH];
        if !resolve_full_path(path, &mut resolved) { write_err("mkdir", "path too long"); return 1; }
        let path_str = match to_str(&resolved) {
            Some(s) => s, None => { write_err("mkdir", "invalid path"); return 1; }
        };
        let ret = sys::mkdir(path_str);
        if sys::is_error(ret) { write_err("mkdir", "failed"); return 1; }
        0
    }
    else if bytes_eq(name, "touch") {
        if cmd.arg_count < 2 { write_err("touch", "missing operand"); return 1; }
        let path = cmd.arg(1);
        let mut resolved = [0u8; MAX_PATH];
        if !resolve_full_path(path, &mut resolved) { write_err("touch", "path too long"); return 1; }
        let path_str = match to_str(&resolved) {
            Some(s) => s, None => { write_err("touch", "invalid path"); return 1; }
        };
        let fd = sys::open(path_str, sys::O_CREAT | sys::O_TRUNC);
        if sys::is_error(fd) { write_err("touch", "failed"); return 1; }
        sys::close(fd as u64);
        0
    }
    else if bytes_eq(name, "rm") {
        if cmd.arg_count < 2 { write_err("rm", "missing operand"); return 1; }
        let path = cmd.arg(1);
        let mut resolved = [0u8; MAX_PATH];
        if !resolve_full_path(path, &mut resolved) { write_err("rm", "path too long"); return 1; }
        let path_str = match to_str(&resolved) {
            Some(s) => s, None => { write_err("rm", "invalid path"); return 1; }
        };
        let ret = sys::unlink(path_str as *const str as *const u8 as u64);
        if sys::is_error(ret) { write_err("rm", "failed"); return 1; }
        0
    }
    else if bytes_eq(name, "pid") {
        write_str("PID: ");
        print_decimal(sys::getpid());
        sys::write(1, b"\n");
        0
    }
    else if bytes_eq(name, "ps") {
        write_str("ps: not yet implemented\n");
        0
    }
    else if bytes_eq(name, "true") { 0 }
    else if bytes_eq(name, "false") { 1 }
    else if bytes_eq(name, "status") {
        unsafe {
            let code = LAST_EXIT_CODE;
            if code < 0 {
                write_str("Signal: ");
                print_decimal((-code) as u64);
            } else {
                write_str("Exit: ");
                print_decimal(code as u64);
            }
            sys::write(1, b"\n");
        }
        0
    }
    else if bytes_eq(name, "jobs") {
        cleanup_done_jobs();
        unsafe {
            let mut found = false;
            for i in 0..MAX_JOBS {
                if JOBS[i].state != JobState::Done {
                    found = true;
                    let prefix = if i == CURRENT_FG { "+" } else { "-" };
                    sys::write(1, b"[");
                    write_str_num(i as u64 + 1);
                    sys::write(1, b"] ");
                    sys::write(1, b"  ");
                    sys::write(1, prefix.as_bytes());
                    sys::write(1, b"  ");
                    let state_str = match JOBS[i].state {
                        JobState::Running => "Running",
                        JobState::Stopped => "Stopped",
                        JobState::Done => "Done",
                    };
                    write_str(state_str);
                    sys::write(1, b"  ");
                    write_str(JOBS[i].name_str());
                    sys::write(1, b"\n");
                }
            }
            if !found {
                write_str("No active jobs\n");
            }
        }
        0
    }
    else if bytes_eq(name, "fg") {
        unsafe {
            let slot = if cmd.arg_count > 1 {
                // Parse job number from argument (e.g., "fg 1")
                let arg = cmd.arg(1);
                let num = parse_decimal(arg);
                if num == 0 || num as usize > MAX_JOBS {
                    write_err("fg", "invalid job number");
                    return 1;
                }
                (num - 1) as usize
            } else {
                // Use current foreground job
                if CURRENT_FG >= MAX_JOBS || JOBS[CURRENT_FG].state == JobState::Done {
                    write_err("fg", "no foreground job");
                    return 1;
                }
                CURRENT_FG
            };

            if slot >= MAX_JOBS || JOBS[slot].state == JobState::Done {
                write_err("fg", "job not found");
                return 1;
            }

            let pgid = JOBS[slot].pgid;
            JOBS[slot].state = JobState::Running;
            CURRENT_FG = slot;

            // Send SIGCONT to resume if stopped
            sys::kill(pgid, 18); // SIGCONT

            // Wait for the job
            loop {
                let result = sys::waitpid(pgid, 1);
                if sys::is_error(result) || result == 0 { break; }
            }

            JOBS[slot].state = JobState::Done;
            CURRENT_FG = 0;
        }
        0
    }
    else if bytes_eq(name, "bg") {
        unsafe {
            let slot = if cmd.arg_count > 1 {
                let arg = cmd.arg(1);
                let num = parse_decimal(arg);
                if num == 0 || num as usize > MAX_JOBS {
                    write_err("bg", "invalid job number");
                    return 1;
                }
                (num - 1) as usize
            } else {
                // Find last stopped job
                let mut found_slot = None;
                for i in (0..MAX_JOBS).rev() {
                    if JOBS[i].state == JobState::Stopped {
                        found_slot = Some(i);
                        break;
                    }
                }
                match found_slot {
                    Some(s) => s,
                    None => {
                        write_err("bg", "no stopped job");
                        return 1;
                    }
                }
            };

            if slot >= MAX_JOBS || JOBS[slot].state != JobState::Stopped {
                write_err("bg", "job not found or not stopped");
                return 1;
            }

            let pgid = JOBS[slot].pgid;
            JOBS[slot].state = JobState::Running;
            sys::kill(pgid, 18); // SIGCONT
            write_str("Background job resumed\n");
        }
        0
    }
    else {
        sys::write(2, b"indosh: unknown command: ");
        sys::write(2, name);
        sys::write(2, b"\n");
        1
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// External command execution
// ═══════════════════════════════════════════════════════════════════════════════

fn exec_external(cmd: &ParsedCmd) -> i64 {
    if cmd.arg_count == 0 { return 1; }

    let name = cmd.arg(0);
    let mut path_buf = [0u8; MAX_PATH];
    if !resolve_path(name, &mut path_buf) { write_err("exec", "path too long"); return 1; }
    let path_str = match to_str(&path_buf) {
        Some(s) => s, None => { write_err("exec", "invalid path"); return 1; }
    };

    if !is_executable(path_str) {
        sys::write(2, b"command not found: ");
        sys::write(2, name);
        sys::write(2, b"\n");
        return 127;
    }

    let mut argv_ptrs: [*const u8; MAX_ARGS] = [core::ptr::null(); MAX_ARGS];
    for i in 0..cmd.arg_count {
        argv_ptrs[i] = cmd.arg(i).as_ptr();
    }

    let pid = sys::fork();
    if sys::is_error(pid) { write_err("exec", "fork failed"); return 1; }

    if pid == 0 {
        // Child: set up redirections
        if cmd.stdin_len > 0 {
            if let Some(f) = to_str(&cmd.stdin_file[..cmd.stdin_len]) {
                let fd = sys::open(f, sys::O_RDONLY);
                if !sys::is_error(fd) { sys::dup2(fd as u64, 0); sys::close(fd as u64); }
                else { sys::write(2, b"redirect: input not found\n"); sys::exit(1); }
            }
        }
        if cmd.stdout_len > 0 {
            if let Some(f) = to_str(&cmd.stdout_file[..cmd.stdout_len]) {
                let flags = if cmd.append_mode {
                    sys::O_WRONLY | sys::O_CREAT | sys::O_APPEND
                } else {
                    sys::O_WRONLY | sys::O_CREAT | sys::O_TRUNC
                };
                let fd = sys::open(f, flags);
                if !sys::is_error(fd) { sys::dup2(fd as u64, 1); sys::close(fd as u64); }
                else { sys::write(2, b"redirect: cannot create\n"); sys::exit(1); }
            }
        }
        let ret = sys::execve(path_str, cmd.arg_count as u64, argv_ptrs.as_ptr());
        sys::write(2, b"exec: failed\n");
        sys::exit(1);
    } else {
        let result = sys::waitpid_blocking(pid as u64);
        if sys::is_error(result) { return 1; }
        result as i64
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Pipeline execution
// ═══════════════════════════════════════════════════════════════════════════════

fn execute_single(cmd: &ParsedCmd) -> i64 {
    if cmd.arg_count == 0 { return 0; }
    let name = cmd.arg(0);
    let has_redirect = cmd.stdin_len > 0 || cmd.stdout_len > 0;
    if is_builtin(name) && !has_redirect {
        return run_builtin(cmd);
    }
    exec_external(cmd)
}

fn execute_pipeline(cmds: &[ParsedCmd], cmd_count: usize) -> i64 {
    if cmd_count == 0 { return 0; }
    if cmd_count == 1 {
        return execute_single(&cmds[0]);
    }

    let mut prev_read_fd: i64 = -1;

    for i in 0..cmd_count {
        let is_last = i == cmd_count - 1;

        let (pipe_r, pipe_w) = if !is_last {
            let p = sys::pipe();
            if sys::is_error(p) { write_err("pipe", "failed"); return 1; }
            ((p >> 32) as i64, (p & 0xFFFFFFFF) as i64)
        } else {
            (-1, -1)
        };

        let pid = sys::fork();
        if sys::is_error(pid) { write_err("pipeline", "fork failed"); return 1; }

        if pid == 0 {
            // Child — set process group to child PID (creates new group)
            sys::setpgid(0, 0);

            if prev_read_fd >= 0 {
                sys::dup2(prev_read_fd as u64, 0);
                sys::close(prev_read_fd as u64);
            }
            if !is_last {
                sys::close(pipe_r as u64);

                sys::dup2(pipe_w as u64, 1);
                sys::close(pipe_w as u64);
            }

            // Explicit redirections override pipes
            let c = &cmds[i];
            if c.stdin_len > 0 {
                if let Some(f) = to_str(&c.stdin_file[..c.stdin_len]) {
                    let fd = sys::open(f, sys::O_RDONLY);
                    if !sys::is_error(fd) { sys::dup2(fd as u64, 0); sys::close(fd as u64); }
                }
            }
            if c.stdout_len > 0 {
                if let Some(f) = to_str(&c.stdout_file[..c.stdout_len]) {
                    let flags = if c.append_mode {
                        sys::O_WRONLY | sys::O_CREAT | sys::O_APPEND
                    } else {
                        sys::O_WRONLY | sys::O_CREAT | sys::O_TRUNC
                    };
                    let fd = sys::open(f, flags);
                    if !sys::is_error(fd) { sys::dup2(fd as u64, 1); sys::close(fd as u64); }
                }
            }

            let name = c.arg(0);
            if is_builtin(name) {
                let code = run_builtin(c);
                sys::exit(code as u64);
            } else {
                let mut path_buf = [0u8; MAX_PATH];
                if !resolve_path(name, &mut path_buf) { sys::exit(1); }
                let path_str = match to_str(&path_buf) {
                    Some(s) => s, None => { sys::exit(1); }
                };
                let mut argv_ptrs: [*const u8; MAX_ARGS] = [core::ptr::null(); MAX_ARGS];
                for j in 0..c.arg_count { argv_ptrs[j] = c.arg(j).as_ptr(); }
                sys::execve(path_str, c.arg_count as u64, argv_ptrs.as_ptr());
                sys::write(2, b"exec failed\n");
                sys::exit(1);
            }
        } else {
            // Parent — set child's process group to child PID
            sys::setpgid(pid as u64, pid as u64);

            if !is_last { sys::close(pipe_w as u64); }
            if prev_read_fd >= 0 { sys::close(prev_read_fd as u64); }
            if !is_last { prev_read_fd = pipe_r; }

            // For single-command pipelines, add to job list
            if i == 0 && cmd_count == 1 {
                let cmd_name = cmds[0].arg(0);
                let name_str = core::str::from_utf8(cmd_name).unwrap_or("unknown");
                unsafe {
                    if let Some(slot) = add_job(pid as u64, name_str) {
                        CURRENT_FG = slot;
                    }
                }
            }
        }
    }

    // Wait for all children
    let mut exit_code = 0i64;
    loop {
        let result = sys::waitpid(0, 1);
        if sys::is_error(result) || result == 0 { break; }
        // Decode POSIX wait status
        let status = result as u64;
        if sys::wifexited(status) {
            exit_code = sys::wexitstatus(status) as i64;
        } else if sys::wifsignaled(status) {
            exit_code = -(sys::wtermsig(status) as i64);
        } else if sys::wifstopped(status) {
            exit_code = -(sys::wstopsig(status) as i64);
        }
    }
    sys::yield_now();
    loop {
        let result = sys::waitpid(0, 1);
        if sys::is_error(result) || result == 0 { break; }
        let status = result as u64;
        if sys::wifexited(status) {
            exit_code = sys::wexitstatus(status) as i64;
        } else if sys::wifsignaled(status) {
            exit_code = -(sys::wtermsig(status) as i64);
        } else if sys::wifstopped(status) {
            exit_code = -(sys::wstopsig(status) as i64);
        }
    }

    // Mark job as done
    unsafe {
        if CURRENT_FG < MAX_JOBS && JOBS[CURRENT_FG].state != JobState::Done {
            JOBS[CURRENT_FG].state = JobState::Done;
        }
        CURRENT_FG = 0;
        LAST_EXIT_CODE = exit_code;
    }

    exit_code
}

// ═══════════════════════════════════════════════════════════════════════════════
// Shell main loop
// ═══════════════════════════════════════════════════════════════════════════════

#[no_mangle]
pub extern "C" fn shell_main(_argc: u64, _argv: u64) -> ! {
    sys::write(1, b"[SHELL] shell_main entered\n");
    write_str("Indominus OS Shell v1.0\n");
    write_str("Type 'help' for commands, 'exit' to quit.\n\n");

    // Trap SIGINT: set handler to ignore (1) so the shell itself is not killed.
    // Child processes inherit the handler but will be killed via send_signal_to_fg.
    sys::sigaction(2, 1, 0); // SIGINT -> ignore
    sys::sigaction(3, 1, 0); // SIGQUIT -> ignore
    sys::sigaction(20, 0, 0); // SIGTSTP -> default (kill)

    let mut input = [0u8; MAX_INPUT];
    let mut tokens = [Token { start: 0, end: 0, kind: TokenKind::Word }; MAX_TOKENS];
    let mut cmds = [ParsedCmd::new(); MAX_CMDS];

    loop {
        sys::write(1, b"[SHELL] loop iteration\n");
        write_str(cwd_str());
        write_str(" $ ");

        let n = sys::read(0, &mut input);

        if sys::is_error(n) || n == 0 {
            continue;
        }
        // Clamp n to buffer length to prevent panic from corrupted return values
        let n = if n as usize > input.len() { input.len() as i64 } else { n };
        let line = &input[..n as usize];

        let mut end = line.len();
        while end > 0 && (line[end - 1] == b'\n' || line[end - 1] == b'\r' || line[end - 1] == b' ') {
            end -= 1;
        }
        let line = &line[..end];
        if line.is_empty() { continue; }

        let token_count = tokenize(line, &mut tokens);
        if token_count == 0 { continue; }

        let cmd_count = parse_pipeline(line, &tokens, token_count, &mut cmds);
        if cmd_count == 0 { continue; }

        execute_pipeline(&cmds, cmd_count);
    }
}
