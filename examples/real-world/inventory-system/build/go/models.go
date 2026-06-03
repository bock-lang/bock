package main

import "fmt"

type Category interface {
	isCategory()
}

type CategoryElectronics struct{}

func (CategoryElectronics) isCategory() {}

type CategoryClothing struct{}

func (CategoryClothing) isCategory() {}

type CategoryFood struct{}

func (CategoryFood) isCategory() {}

type CategoryBooks struct{}

func (CategoryBooks) isCategory() {}

type CategoryOther struct{}

func (CategoryOther) isCategory() {}

type Product struct {
	Id	int64
	Name	string
	Category	Category
	Price	float64
	Quantity	int64
}

func (self *Product) InStock() bool {
	return (self.Quantity > 0)
}

func (self *Product) StockValue() float64 {
	return (self.Price * self.Quantity.toFloat())
}

func (self *Product) Display() string {
	return fmt.Sprintf("%v (x%v) @ ${self.price}", self.Name, self.Quantity)
}

type InventorySummary struct {
	TotalProducts	int64
	TotalValue	float64
	OutOfStock	int64
}

func CategoryName(cat Category) string {
	return func() string { switch cat.(type) { case CategoryElectronics: return "Electronics"; case CategoryClothing: return "Clothing"; case CategoryFood: return "Food"; case CategoryBooks: return "Books"; case CategoryOther: return "Other"; }; panic("unreachable") }()
}
