package main

import "fmt"

type DataPoint struct {
	Name	string
	Value	float64
	Category	string
}

type Summary struct {
	Count	int64
	Total	float64
	Average	float64
}

type Report struct {
	Title	string
	Body	string
}

func Normalize(data []DataPoint) []DataPoint {
	maxVal := 0.0
	for _, dp := range data {
		if (dp.Value > maxVal) {
			maxVal = dp.Value
		}
	}
	return func() []DataPoint { if (maxVal > 0.0) { return data.map(func(dp interface{}) DataPoint { return DataPoint{Name: dp.Name, Value: (dp.Value / maxVal), Category: dp.Category} }) } else { return data } }()
}

func Scale(factor float64, data []DataPoint) []DataPoint {
	return data.map(func(dp interface{}) DataPoint { return DataPoint{Name: dp.Name, Value: (dp.Value * factor), Category: dp.Category} })
}

func FilterCategory(cat string) func([]DataPoint) []DataPoint {
	return func(data interface{}) interface{} { return data.filter(func(dp interface{}) bool { return (dp.Category == cat) }) }
}

func RemoveInvalid(data []DataPoint) []DataPoint {
	return data.filter(func(dp interface{}) bool { return (dp.Value > 0.0) })
}

func ComputeSummary(data []DataPoint) Summary {
	total := 0.0
	count := 0
	fcount := 0.0
	for _, dp := range data {
		total = (total + dp.Value)
		count = (count + 1)
		fcount = (fcount + 1.0)
	}
	avg := func() Summary { if (fcount > 0.0) { return (total / fcount) } else { return 0.0 } }()
	return Summary{Count: count, Total: total, Average: avg}
}

func FormatSummary(s Summary) string {
	return fmt.Sprintf("Items: %v, Total: %v, Average: %v", s.Count, s.Total, s.Average)
}

func FormatData(data []DataPoint) string {
	result := ""
	first := true
	for _, dp := range data {
		if first {
			result = fmt.Sprintf("  %v: %v [%v]", dp.Name, dp.Value, dp.Category)
			first = false
		} else {
			result = (result + fmt.Sprintf("\n  %v: %v [%v]", dp.Name, dp.Value, dp.Category))
		}
	}
	return result
}

func BuildReportPipeline() func([]DataPoint) string {
	return func(composeX interface{}) string { return FormatSummary(func(composeX interface{}) Summary { return ComputeSummary(Normalize(composeX)) }(composeX)) }
}

func ApplyPipeline(title string, data []DataPoint, pipeline func([]DataPoint) string) Report {
	body := pipeline(data)
	return Report{Title: title, Body: body}
}

func PrintReport(report Report) {
	fmt.Println(fmt.Sprintf("--- %v ---", report.Title))
	fmt.Println(report.Body)
	fmt.Println("")
}

func main() {
	var data []DataPoint = []DataPoint{DataPoint{Name: "alpha", Value: 10.0, Category: "sensor"}, DataPoint{Name: "beta", Value: 25.0, Category: "sensor"}, DataPoint{Name: "gamma", Value: 5.0, Category: "manual"}, DataPoint{Name: "delta", Value: 40.0, Category: "sensor"}, DataPoint{Name: "epsilon", Value: 15.0, Category: "manual"}, DataPoint{Name: "zeta", Value: 0.0, Category: "sensor"}, DataPoint{Name: "eta", Value: 30.0, Category: "manual"}}
	fmt.Println("=== Data Pipeline Demo ===")
	fmt.Println("")
	fmt.Println("--- Raw Data Summary ---")
	rawSummary := FormatSummary(ComputeSummary(data))
	fmt.Println(rawSummary)
	fmt.Println("")
	fmt.Println("--- Cleaned + Normalized ---")
	cleaned := Normalize(RemoveInvalid(data))
	cleanedListing := FormatData(cleaned)
	fmt.Println(cleanedListing)
	fmt.Println("")
	sensorFilter := FilterCategory("sensor")
	manualFilter := FilterCategory("manual")
	sensorData := sensorFilter(data)
	manualData := manualFilter(data)
	fmt.Println("--- Sensor Data Summary ---")
	sensorSummary := FormatSummary(ComputeSummary(RemoveInvalid(sensorData)))
	fmt.Println(sensorSummary)
	fmt.Println("")
	fmt.Println("--- Manual Data Summary ---")
	manualSummary := FormatSummary(ComputeSummary(manualData))
	fmt.Println(manualSummary)
	fmt.Println("")
	reportPipeline := BuildReportPipeline()
	fullReport := ApplyPipeline("Full Dataset Report", data, reportPipeline)
	PrintReport(fullReport)
	sensorReport := ApplyPipeline("Sensor Report", sensorData, reportPipeline)
	PrintReport(sensorReport)
	manualReport := ApplyPipeline("Manual Report", manualData, reportPipeline)
	PrintReport(manualReport)
	fmt.Println("--- Scaled Sensor Summary ---")
	scaledResult := FormatSummary(ComputeSummary(Normalize(RemoveInvalid(sensorData))))
	fmt.Println(scaledResult)
	fmt.Println("")
	fmt.Println("=== Pipeline Complete ===")
}
