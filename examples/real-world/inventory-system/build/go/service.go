package main

import "fmt"

func FindByCategory(products []Product, cat Category) []Product {
	return products.filter(func(p interface{}) bool { return (CategoryName(p.Category) == CategoryName(cat)) })
}

func FindOutOfStock(products []Product) []Product {
	return products.filter(func(p interface{}) bool { return (p.InStock() == false) })
}

func FindInStock(products []Product) []Product {
	return products.filter(func(p interface{}) interface{} { return p.InStock() })
}

func TotalValue(products []Product) float64 {
	values := products.map(func(p interface{}) interface{} { return p.StockValue() })
	return values.fold(0.0, func(acc interface{}, v interface{}) interface{} { return (acc + v) })
}

func Summarize(products []Product) InventorySummary {
	total := int64(len(products))
	value := TotalValue(products)
	oos := int64(len(FindOutOfStock(products)))
	return InventorySummary{TotalProducts: total, TotalValue: value, OutOfStock: oos}
}

func FormatSummary(summary InventorySummary) string {
	return fmt.Sprintf("Inventory: %v products, ${summary.total_value} total value, %v out of stock", summary.TotalProducts, summary.OutOfStock)
}

func Restock(product Product, amount int64) Product {
	return Product{Id: product.Id, Name: product.Name, Category: product.Category, Price: product.Price, Quantity: (product.Quantity + amount)}
}
