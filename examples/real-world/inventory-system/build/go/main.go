package main

import "fmt"

func main() {
	products := []Product{Product{Id: 1, Name: "Laptop", Category: CategoryElectronics{}, Price: 999.99, Quantity: 5}, Product{Id: 2, Name: "T-Shirt", Category: CategoryClothing{}, Price: 19.99, Quantity: 100}, Product{Id: 3, Name: "Rice", Category: CategoryFood{}, Price: 4.50, Quantity: 0}, Product{Id: 4, Name: "Novel", Category: CategoryBooks{}, Price: 12.99, Quantity: 25}, Product{Id: 5, Name: "Headphones", Category: CategoryElectronics{}, Price: 49.99, Quantity: 0}}
	fmt.Println("=== Inventory ===")
	for _, p := range products {
		fmt.Println(p.Display())
	}
	summary := Summarize(products)
	fmt.Println("")
	fmt.Println(FormatSummary(summary))
	electronics := FindByCategory(products, CategoryElectronics{})
	fmt.Println("")
	fmt.Println(fmt.Sprintf("=== Electronics (%v) ===", int64(len(electronics))))
	for _, p := range electronics {
		fmt.Println(p.Display())
	}
	oos := FindOutOfStock(products)
	fmt.Println("")
	fmt.Println(fmt.Sprintf("=== Out of Stock (%v) ===", int64(len(oos))))
	for _, p := range oos {
		fmt.Println(p.Display())
	}
	rice := Product{Id: 3, Name: "Rice", Category: CategoryFood{}, Price: 4.50, Quantity: 0}
	restocked := Restock(rice, 50)
	fmt.Println("")
	fmt.Println(fmt.Sprintf("Restocked: %v", restocked.Display()))
}
