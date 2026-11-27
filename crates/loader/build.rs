#[path = "winres.rs"] mod winres;

fn main() {
   winres::embed(
      winres::Kind::Dll,
      "phoenix.dll",
      "Ruby runtime for Discord Phoenix",
      None,
   );
}
