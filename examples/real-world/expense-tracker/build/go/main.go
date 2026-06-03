package main

import "fmt"

type Category interface {
	isCategory()
}

type CategoryFood struct{}

func (CategoryFood) isCategory() {}

type CategoryTransport struct{}

func (CategoryTransport) isCategory() {}

type CategoryHousing struct{}

func (CategoryHousing) isCategory() {}

type CategoryEntertainment struct{}

func (CategoryEntertainment) isCategory() {}

type CategoryUtilities struct{}

func (CategoryUtilities) isCategory() {}

type CategoryOther struct{}

func (CategoryOther) isCategory() {}

type Expense struct {
	Id	int64
	Amount	float64
	Category	Category
	Description	string
	Date	string
}

type Report struct {
	Total	float64
	ByCategory	map[string]float64
	Count	int64
}

func CategoryName(cat Category) string {
	return func() string { switch cat.(type) { case CategoryFood: return "Food"; case CategoryTransport: return "Transport"; case CategoryHousing: return "Housing"; case CategoryEntertainment: return "Entertainment"; case CategoryUtilities: return "Utilities"; case CategoryOther: return "Other"; }; panic("unreachable") }()
}

func IsCategory(expense Expense, cat Category) bool {
	return func() bool { switch expense.Category.(type) { case CategoryFood: return func() bool { switch cat.(type) { case CategoryFood: return true; default: return false; }; panic("unreachable") }(); case CategoryTransport: return func() bool { switch cat.(type) { case CategoryTransport: return true; default: return false; }; panic("unreachable") }(); case CategoryHousing: return func() bool { switch cat.(type) { case CategoryHousing: return true; default: return false; }; panic("unreachable") }(); case CategoryEntertainment: return func() bool { switch cat.(type) { case CategoryEntertainment: return true; default: return false; }; panic("unreachable") }(); case CategoryUtilities: return func() bool { switch cat.(type) { case CategoryUtilities: return true; default: return false; }; panic("unreachable") }(); case CategoryOther: return func() bool { switch cat.(type) { case CategoryOther: return true; default: return false; }; panic("unreachable") }(); }; panic("unreachable") }()
}

func AddExpense(expenses []Expense, expense Expense) []Expense {
	return (expenses + []Expense{expense})
}

func RemoveExpense(expenses []Expense, id int64) []Expense {
	return expenses.filter(func(e interface{}) bool { return (e.Id != id) })
}

func FindByCategory(expenses []Expense, cat Category) []Expense {
	return expenses.filter(func(e interface{}) bool { return IsCategory(e, cat) })
}

func TotalSpending(expenses []Expense) float64 {
	total := 0.0
	for _, e := range expenses {
		total = (total + e.Amount)
	}
	return total
}

func CategoryTotal(expenses []Expense, cat Category) float64 {
	total := 0.0
	for _, e := range expenses {
		if IsCategory(e, cat) {
			total = (total + e.Amount)
		}
	}
	return total
}

func SpendingByCategory(expenses []Expense) map[string]float64 {
	food := CategoryTotal(expenses, CategoryFood{})
	transport := CategoryTotal(expenses, CategoryTransport{})
	housing := CategoryTotal(expenses, CategoryHousing{})
	entertainment := CategoryTotal(expenses, CategoryEntertainment{})
	utilities := CategoryTotal(expenses, CategoryUtilities{})
	other := CategoryTotal(expenses, CategoryOther{})
	return map[string]float64{"Food": food, "Transport": transport, "Housing": housing, "Entertainment": entertainment, "Utilities": utilities, "Other": other}
}

func GenerateReport(expenses []Expense) Report {
	total := TotalSpending(expenses)
	byCat := SpendingByCategory(expenses)
	count := int64(len(expenses))
	return Report{Total: total, ByCategory: byCat, Count: count}
}

func FormatReport(report Report) string {
	header := "=== Expense Report ==="
	summary := fmt.Sprintf("Total: %v | Items: %v", report.Total, report.Count)
	catKeys := func(__m map[interface{}]interface{}) []interface{} { __r := make([]interface{}, 0, len(__m)); for __mk := range __m { __r = append(__r, __mk) }; return __r }(report.ByCategory)
	lines := ((header + "\n") + summary)
	for _, key := range catKeys {
		val := func(__m map[interface{}]interface{}, __k interface{}) __bockOption { if __v, __ok := __m[__k]; __ok { return __bockSome(__v) }; return __bockNone }(report.ByCategory, key)
		__opt := val
		if __opt.tag == "Some" { amount := __opt.v; _ = amount; 
			lines = (lines + fmt.Sprintf("\n  %v: %v", key, amount))
		} else { 
			// empty
		}
	}
	return lines
}

func main() {
	fmt.Println("=== Expense Tracker Demo ===")
	fmt.Println("")
	var expenses []Expense = []Expense{}
	expenses = AddExpense(expenses, Expense{Id: 1, Amount: 45.50, Category: CategoryFood{}, Description: "Grocery shopping", Date: "2026-03-01"})
	expenses = AddExpense(expenses, Expense{Id: 2, Amount: 120.00, Category: CategoryHousing{}, Description: "Electric bill", Date: "2026-03-02"})
	expenses = AddExpense(expenses, Expense{Id: 3, Amount: 30.00, Category: CategoryTransport{}, Description: "Bus pass", Date: "2026-03-03"})
	expenses = AddExpense(expenses, Expense{Id: 4, Amount: 15.99, Category: CategoryEntertainment{}, Description: "Movie ticket", Date: "2026-03-05"})
	expenses = AddExpense(expenses, Expense{Id: 5, Amount: 60.00, Category: CategoryUtilities{}, Description: "Internet service", Date: "2026-03-06"})
	expenses = AddExpense(expenses, Expense{Id: 6, Amount: 22.75, Category: CategoryFood{}, Description: "Lunch out", Date: "2026-03-07"})
	fmt.Println(fmt.Sprintf("All expenses (%v):", int64(len(expenses))))
	for _, e := range expenses {
		fmt.Println(fmt.Sprintf("  #%v %v: %v [%v]", e.Id, e.Description, e.Amount, CategoryName(e.Category)))
	}
	expenses = RemoveExpense(expenses, 4)
	fmt.Println("")
	fmt.Println(fmt.Sprintf("After removing #4: %v expenses", int64(len(expenses))))
	foodItems := FindByCategory(expenses, CategoryFood{})
	fmt.Println("")
	fmt.Println(fmt.Sprintf("Food expenses (%v):", int64(len(foodItems))))
	for _, e := range foodItems {
		fmt.Println(fmt.Sprintf("  %v: %v", e.Description, e.Amount))
	}
	report := GenerateReport(expenses)
	fmt.Println("")
	formatted := FormatReport(report)
	fmt.Println(formatted)
	fmt.Println("")
	fmt.Println("=== Done ===")
}
