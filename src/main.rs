use std::env;
use std::process;

fn print_usage() {
    eprintln!("Usage: mmreg [options] <read|write> <address> [value]");
    eprintln!("Options:");
    eprintln!("  -f, --force               Force mapping even if address validation fails");
    eprintln!("Commands:");
    eprintln!("  read  <address>           Read 32-bit value at address");
    eprintln!("  write <address> <value>   Write 32-bit value to address");
    eprintln!("Examples:");
    eprintln!("  mmreg read 0x4000_0000");
    eprintln!("  mmreg write 0x4000_0000 0xDEADBEEF");
    eprintln!("  mmreg --force read 0x4000_0000");
}

fn main() {
    let args: Vec<String> = env::args().collect();
    
    // Parse options
    let mut force_map = false;
    let mut cmd_idx = 1;
    
    while cmd_idx < args.len() {
        match args[cmd_idx].as_str() {
            "-f" | "--force" => {
                force_map = true;
                cmd_idx += 1;
            }
            "-h" | "--help" => {
                print_usage();
                process::exit(0);
            }
            arg if !arg.starts_with('-') => {
                // Found non-option argument, stop parsing options
                break;
            }
            arg => {
                eprintln!("Unknown option: {}", arg);
                print_usage();
                process::exit(1);
            }
        }
    }

    // Validate remaining arguments
    if cmd_idx >= args.len() {
        print_usage();
        process::exit(1);
    }

    let command = &args[cmd_idx];
    if cmd_idx + 1 >= args.len() {
        eprintln!("Missing address argument");
        print_usage();
        process::exit(1);
    }

    let address = match u64::from_str_radix(args[cmd_idx + 1].trim_start_matches("0x"), 16) {
        Ok(addr) => addr,
        Err(_) => {
            eprintln!("Invalid address: {}", args[cmd_idx + 1]);
            process::exit(1);
        }
    };

    match command.as_str() {
        "read" => match mmreg::read_register_at(address, force_map) {
            Ok(val) => println!("0x{:08X}", val),
            Err(e) => {
                eprintln!("Read error: {}", e);
                process::exit(1);
            }
        },
        "write" => {
            if cmd_idx + 2 >= args.len() {
                eprintln!("Missing value argument for write");
                print_usage();
                process::exit(1);
            }
            let value = match u32::from_str_radix(args[cmd_idx + 2].trim_start_matches("0x"), 16) {
                Ok(v) => v,
                Err(_) => {
                    eprintln!("Invalid value: {}", args[cmd_idx + 2]);
                    process::exit(1);
                }
            };
            match mmreg::write_register_at(address, value, force_map) {
                Ok(_) => println!("Wrote 0x{:08X} to 0x{:X}", value, address),
                Err(e) => {
                    eprintln!("Write error: {}", e);
                    process::exit(1);
                }
            }
        }
        _ => {
            eprintln!("Unknown command: {}", command);
            print_usage();
            process::exit(1);
        }
    }
}
