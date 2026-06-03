import type { BockOption } from "./_bock_runtime.js";
export type Category = Category_Food | Category_Transport | Category_Housing | Category_Entertainment | Category_Utilities | Category_Other;

interface Category_Food { readonly _tag: "Food"; }
const Category_Food: Category_Food = Object.freeze({ _tag: "Food" as const });
interface Category_Transport { readonly _tag: "Transport"; }
const Category_Transport: Category_Transport = Object.freeze({ _tag: "Transport" as const });
interface Category_Housing { readonly _tag: "Housing"; }
const Category_Housing: Category_Housing = Object.freeze({ _tag: "Housing" as const });
interface Category_Entertainment { readonly _tag: "Entertainment"; }
const Category_Entertainment: Category_Entertainment = Object.freeze({ _tag: "Entertainment" as const });
interface Category_Utilities { readonly _tag: "Utilities"; }
const Category_Utilities: Category_Utilities = Object.freeze({ _tag: "Utilities" as const });
interface Category_Other { readonly _tag: "Other"; }
const Category_Other: Category_Other = Object.freeze({ _tag: "Other" as const });

export class Expense {
  id: number;
  amount: number;
  category: Category;
  description: string;
  date: string;
  constructor({ id, amount, category, description, date }: { id: number; amount: number; category: Category; description: string; date: string }) {
    this.id = id;
    this.amount = amount;
    this.category = category;
    this.description = description;
    this.date = date;
  }
}

export class Report {
  total: number;
  by_category: Map<string, number>;
  count: number;
  constructor({ total, by_category, count }: { total: number; by_category: Map<string, number>; count: number }) {
    this.total = total;
    this.by_category = by_category;
    this.count = count;
  }
}

export function categoryName(cat: Category): string {
  return (() => {
    switch (cat._tag) {
      case "Food": {
        return "Food";
        break;
      }
      case "Transport": {
        return "Transport";
        break;
      }
      case "Housing": {
        return "Housing";
        break;
      }
      case "Entertainment": {
        return "Entertainment";
        break;
      }
      case "Utilities": {
        return "Utilities";
        break;
      }
      case "Other": {
        return "Other";
        break;
      }
    }
  })();
}

export function isCategory(expense: Expense, cat: Category): boolean {
  return (() => {
    const __match1 = expense.category;
    switch (__match1._tag) {
      case "Food": {
        return (() => {
          switch (cat._tag) {
            case "Food": {
              return true;
              break;
            }
            default: {
              return false;
              break;
            }
          }
        })();
        break;
      }
      case "Transport": {
        return (() => {
          switch (cat._tag) {
            case "Transport": {
              return true;
              break;
            }
            default: {
              return false;
              break;
            }
          }
        })();
        break;
      }
      case "Housing": {
        return (() => {
          switch (cat._tag) {
            case "Housing": {
              return true;
              break;
            }
            default: {
              return false;
              break;
            }
          }
        })();
        break;
      }
      case "Entertainment": {
        return (() => {
          switch (cat._tag) {
            case "Entertainment": {
              return true;
              break;
            }
            default: {
              return false;
              break;
            }
          }
        })();
        break;
      }
      case "Utilities": {
        return (() => {
          switch (cat._tag) {
            case "Utilities": {
              return true;
              break;
            }
            default: {
              return false;
              break;
            }
          }
        })();
        break;
      }
      case "Other": {
        return (() => {
          switch (cat._tag) {
            case "Other": {
              return true;
              break;
            }
            default: {
              return false;
              break;
            }
          }
        })();
        break;
      }
    }
  })();
}

export function addExpense(expenses: Array<Expense>, expense: Expense): Array<Expense> {
  return (expenses + [expense]);
}

export function removeExpense(expenses: Array<Expense>, id: number): Array<Expense> {
  return expenses.filter(expenses, (e) => (e.id !== id));
}

export function findByCategory(expenses: Array<Expense>, cat: Category): Array<Expense> {
  return expenses.filter(expenses, (e) => isCategory(e, cat));
}

export function totalSpending(expenses: Array<Expense>): number {
  let total = 0.0;
  for (const e of expenses) {
    total = (total + e.amount);
  }
  return total;
}

export function categoryTotal(expenses: Array<Expense>, cat: Category): number {
  let total = 0.0;
  for (const e of expenses) {
    if (isCategory(e, cat)) {
      total = (total + e.amount);
    }
  }
  return total;
}

export function spendingByCategory(expenses: Array<Expense>): Map<string, number> {
  const food = categoryTotal(expenses, Category_Food);
  const transport = categoryTotal(expenses, Category_Transport);
  const housing = categoryTotal(expenses, Category_Housing);
  const entertainment = categoryTotal(expenses, Category_Entertainment);
  const utilities = categoryTotal(expenses, Category_Utilities);
  const other = categoryTotal(expenses, Category_Other);
  return new Map([["Food", food], ["Transport", transport], ["Housing", housing], ["Entertainment", entertainment], ["Utilities", utilities], ["Other", other]]);
}

export function generateReport(expenses: Array<Expense>): Report {
  const total = totalSpending(expenses);
  const byCat = spendingByCategory(expenses);
  const count = (expenses).length;
  return new Report({ total: total, by_category: byCat, count: count });
}

export function formatReport(report: Report): string {
  const header = "=== Expense Report ===";
  const summary = `Total: ${report.total} | Items: ${report.count}`;
  const catKeys = [...(report.by_category).keys()];
  let lines = ((header + "\n") + summary);
  for (const key of catKeys) {
    const val = (<K, V>(__m: Map<K, V>, __k: K): BockOption<V> => __m.has(__k) ? { _tag: "Some" as const, _0: __m.get(__k)! } : { _tag: "None" as const })(report.by_category, key);
    switch (val._tag) {
      case "Some": {
        const amount = val._0;
        lines = (lines + `
  ${key}: ${amount}`);
        break;
      }
      case "None": {
        break;
      }
    }
  }
  return lines;
}

function main() {
  console.log("=== Expense Tracker Demo ===");
  console.log("");
  let expenses: Array<Expense> = [];
  expenses = addExpense(expenses, new Expense({ id: 1, amount: 45.50, category: Category_Food, description: "Grocery shopping", date: "2026-03-01" }));
  expenses = addExpense(expenses, new Expense({ id: 2, amount: 120.00, category: Category_Housing, description: "Electric bill", date: "2026-03-02" }));
  expenses = addExpense(expenses, new Expense({ id: 3, amount: 30.00, category: Category_Transport, description: "Bus pass", date: "2026-03-03" }));
  expenses = addExpense(expenses, new Expense({ id: 4, amount: 15.99, category: Category_Entertainment, description: "Movie ticket", date: "2026-03-05" }));
  expenses = addExpense(expenses, new Expense({ id: 5, amount: 60.00, category: Category_Utilities, description: "Internet service", date: "2026-03-06" }));
  expenses = addExpense(expenses, new Expense({ id: 6, amount: 22.75, category: Category_Food, description: "Lunch out", date: "2026-03-07" }));
  console.log(`All expenses (${(expenses).length}):`);
  for (const e of expenses) {
    return console.log(`  #${e.id} ${e.description}: ${e.amount} [${categoryName(e.category)}]`);
  }
  expenses = removeExpense(expenses, 4);
  console.log("");
  console.log(`After removing #4: ${(expenses).length} expenses`);
  const foodItems = findByCategory(expenses, Category_Food);
  console.log("");
  console.log(`Food expenses (${(foodItems).length}):`);
  for (const e of foodItems) {
    return console.log(`  ${e.description}: ${e.amount}`);
  }
  const report = generateReport(expenses);
  console.log("");
  const formatted = formatReport(report);
  console.log(formatted);
  console.log("");
  return console.log("=== Done ===");
}
export { Category_Entertainment, Category_Food, Category_Housing, Category_Other, Category_Transport, Category_Utilities };
main();
//# sourceMappingURL=main.ts.map
