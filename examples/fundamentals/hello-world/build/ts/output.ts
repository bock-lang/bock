function greet(name: string): string {
  return `Hello, ${name}!`;
}

function banner(title: string, subtitle: string): string {
  return `=== ${title} === ${subtitle}`;
}

function main() {
  console.log("Hello, World!");
  const name = "Bock";
  console.log(`Welcome to ${name}!`);
  console.log(greet("World"));
  console.log(greet("Bock Developer"));
  const heading = banner("Bock Language", "Simple. Declarative. Powerful.");
  console.log(heading);
  const language = "Bock";
  const version = "0.1.0";
  return console.log(`${language} v${version} — ready to go!`);
}
