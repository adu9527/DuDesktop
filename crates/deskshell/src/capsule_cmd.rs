use render::CapsuleOptions;

pub fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut options = CapsuleOptions::default();
    let mut arguments = args.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--self-test" => options.self_test = true,
            "--input-test" => options.input_test = true,
            "--recovery-test" => options.recovery_test = true,
            "--seconds" => {
                let value = arguments.next().ok_or("--seconds 需要秒数")?;
                options.seconds = Some(value.parse()?);
            }
            "--monitor" => {
                let value = arguments.next().ok_or("--monitor 需要索引")?;
                options.monitor = Some(value.parse()?);
            }
            other => return Err(format!("未知参数：{other}").into()),
        }
    }
    if options.input_test || options.recovery_test {
        options.self_test = true;
    }
    render::run_capsule_prototype(options)?;
    Ok(())
}
