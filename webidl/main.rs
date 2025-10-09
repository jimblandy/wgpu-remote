use weedle::Parse;

fn main() {
    let source = include_str!("webgpu.idl");
    let (remaining, parsed) =
        weedle::Definitions::parse(source).expect("failed to parse `Definitions`");
    println!("parsed: {parsed:#?}", );
    println!();
    println!("remaining: {remaining:?}");
}
