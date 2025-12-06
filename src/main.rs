use std::env;
use std::process;

fn print_usage() {
	eprintln!("Usage: mmreg <read|write> <address> [value]");
	eprintln!("  read  <address>           Read 32-bit value at address");
	eprintln!("  write <address> <value>   Write 32-bit value to address");
}

fn main() {
	let args: Vec<String> = env::args().collect();
	if args.len() < 3 {
		print_usage();
		process::exit(1);
	}

	let command = &args[1];
	let address = match u64::from_str_radix(&args[2].trim_start_matches("0x"), 16) {
		Ok(addr) => addr,
		Err(_) => {
			eprintln!("Invalid address: {}", args[2]);
			process::exit(1);
		}
	};

	match command.as_str() {
		"read" => {
			match mmreg::read_register_at(address) {
				Ok(val) => println!("0x{:08X}", val),
				Err(e) => {
					eprintln!("Read error: {}", e);
					process::exit(1);
				}
			}
		}
		"write" => {
			if args.len() < 4 {
				print_usage();
				process::exit(1);
			}
			let value = match u32::from_str_radix(&args[3].trim_start_matches("0x"), 16) {
				Ok(v) => v,
				Err(_) => {
					eprintln!("Invalid value: {}", args[3]);
					process::exit(1);
				}
			};
			match mmreg::write_register_at(address, value) {
				Ok(_) => println!("Wrote 0x{:08X} to 0x{:X}", value, address),
				Err(e) => {
					eprintln!("Write error: {}", e);
					process::exit(1);
				}
			}
		}
		_ => {
			print_usage();
			process::exit(1);
		}
	}
}
