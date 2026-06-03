from __future__ import annotations
from typing import Callable
from dataclasses import dataclass

@dataclass
class DataPoint:
    name: str
    value: float
    category: str

@dataclass
class Summary:
    count: int
    total: float
    average: float

@dataclass
class Report:
    title: str
    body: str

def normalize(data: list[DataPoint]) -> list[DataPoint]:
    max_val = 0.0
    for dp in data:
        if (dp.value > max_val):
            max_val = dp.value
    return (data.map(lambda dp: DataPoint(name=dp.name, value=(dp.value / max_val), category=dp.category)) if (max_val > 0.0) else data)

def scale(factor: float, data: list[DataPoint]) -> list[DataPoint]:
    return data.map(lambda dp: DataPoint(name=dp.name, value=(dp.value * factor), category=dp.category))

def filter_category(cat: str) -> Callable[[list[DataPoint]], list[DataPoint]]:
    return lambda data: data.filter(lambda dp: (dp.category == cat))

def remove_invalid(data: list[DataPoint]) -> list[DataPoint]:
    return data.filter(lambda dp: (dp.value > 0.0))

def compute_summary(data: list[DataPoint]) -> Summary:
    total = 0.0
    count = 0
    fcount = 0.0
    for dp in data:
        total = (total + dp.value)
        count = (count + 1)
        fcount = (fcount + 1.0)
    avg = ((total / fcount) if (fcount > 0.0) else 0.0)
    return Summary(count=count, total=total, average=avg)

def format_summary(s: Summary) -> str:
    return f"Items: {s.count}, Total: {s.total}, Average: {s.average}"

def format_data(data: list[DataPoint]) -> str:
    result = ""
    first = True
    for dp in data:
        if first:
            result = f"  {dp.name}: {dp.value} [{dp.category}]"
            first = False
        else:
            result = (result + f"""
  {dp.name}: {dp.value} [{dp.category}]""")
    return result

def build_report_pipeline() -> Callable[[list[DataPoint]], str]:
    return lambda __compose_x: format_summary(lambda __compose_x: compute_summary(normalize(__compose_x))(__compose_x))

def apply_pipeline(title: str, data: list[DataPoint], pipeline: Callable[[list[DataPoint]], str]) -> Report:
    body = pipeline(data)
    return Report(title=title, body=body)

def print_report(report: Report) -> None:
    print(f"--- {report.title} ---")
    print(report.body)
    return print("")

def main():
    data: list[DataPoint] = [DataPoint(name="alpha", value=10.0, category="sensor"), DataPoint(name="beta", value=25.0, category="sensor"), DataPoint(name="gamma", value=5.0, category="manual"), DataPoint(name="delta", value=40.0, category="sensor"), DataPoint(name="epsilon", value=15.0, category="manual"), DataPoint(name="zeta", value=0.0, category="sensor"), DataPoint(name="eta", value=30.0, category="manual")]
    print("=== Data Pipeline Demo ===")
    print("")
    print("--- Raw Data Summary ---")
    raw_summary = format_summary(compute_summary(data))
    print(raw_summary)
    print("")
    print("--- Cleaned + Normalized ---")
    cleaned = normalize(remove_invalid(data))
    cleaned_listing = format_data(cleaned)
    print(cleaned_listing)
    print("")
    sensor_filter = filter_category("sensor")
    manual_filter = filter_category("manual")
    sensor_data = sensor_filter(data)
    manual_data = manual_filter(data)
    print("--- Sensor Data Summary ---")
    sensor_summary = format_summary(compute_summary(remove_invalid(sensor_data)))
    print(sensor_summary)
    print("")
    print("--- Manual Data Summary ---")
    manual_summary = format_summary(compute_summary(manual_data))
    print(manual_summary)
    print("")
    report_pipeline = build_report_pipeline()
    full_report = apply_pipeline("Full Dataset Report", data, report_pipeline)
    print_report(full_report)
    sensor_report = apply_pipeline("Sensor Report", sensor_data, report_pipeline)
    print_report(sensor_report)
    manual_report = apply_pipeline("Manual Report", manual_data, report_pipeline)
    print_report(manual_report)
    print("--- Scaled Sensor Summary ---")
    scaled_result = format_summary(compute_summary(normalize(remove_invalid(sensor_data))))
    print(scaled_result)
    print("")
    return print("=== Pipeline Complete ===")
if __name__ == "__main__":
    main()
