use crate::print_command_help;

pub(crate) fn cmd_ops(args: &[String]) -> i32 {
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "-h" | "--help" => {
                print_command_help("ops");
                return 0;
            }
            other => {
                eprintln!("error: unknown option for ops: {other}");
                return 2;
            }
        }
    }
    if json {
        println!("{}", csvx_core::contract::operations_json());
    } else {
        println!("operations ({}):", csvx_core::contract::OPERATIONS.len());
        for op in csvx_core::contract::OPERATIONS {
            println!("{:<18} {:<8} {}", op.name, op.effect.as_str(), op.summary);
        }
        println!("error codes: {}", csvx_core::contract::ERROR_CODES.len());
    }
    0
}
