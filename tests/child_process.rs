use std::io::{self, BufRead};

fn main() {
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();

    println!("Starting...");
    eprintln!("Error message");

    while let Some(Ok(line)) = lines.next() {
        match line.as_str() {
            "exit" => {
                std::process::exit(0);
            }
            "error" => {
                println!("Exiting with error...");
                std::process::exit(42);
            }
            line => {
                println!("Echo: {}", line);
            }
        }
    }

    // Sometimes, when the process receives a SIGTERM that's not handled, the runtime will close
    // stdin while the main thread is still running. In this case, the process could end up here
    // and exit with status code 0 before the runtime has time to terminate it with status code 143
    // (128 + 15 (SIGTERM))! So we enter an infinite loop here to prevent that: the child process
    // will wait here until the runtime comes and terminates it with status code 143.
    loop {}
}
