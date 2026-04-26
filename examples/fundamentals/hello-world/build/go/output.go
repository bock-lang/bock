package main

import "fmt"

func greet(name string) string {
	return fmt.Sprintf("Hello, %v!", name)
}

func banner(title string, subtitle string) string {
	return fmt.Sprintf("=== %v === %v", title, subtitle)
}

func main() {
	fmt.Println("Hello, World!")
	name := "Bock"
	fmt.Println(fmt.Sprintf("Welcome to %v!", name))
	fmt.Println(greet("World"))
	fmt.Println(greet("Bock Developer"))
	heading := banner("Bock Language", "Simple. Declarative. Powerful.")
	fmt.Println(heading)
	language := "Bock"
	version := "0.1.0"
	fmt.Println(fmt.Sprintf("%v v%v — ready to go!", language, version))
}
