class DataPoint {
  constructor({ name, value, category }) {
    this.name = name;
    this.value = value;
    this.category = category;
  }
}

class Summary {
  constructor({ count, total, average }) {
    this.count = count;
    this.total = total;
    this.average = average;
  }
}

class Report {
  constructor({ title, body }) {
    this.title = title;
    this.body = body;
  }
}

export function normalize(data) {
  let maxVal = 0.0;
  for (const dp of data) {
    if ((dp.value > maxVal)) {
      maxVal = dp.value;
    }
  }
  return ((maxVal > 0.0) ? data.map(data, (dp) => new DataPoint({ name: dp.name, value: (dp.value / maxVal), category: dp.category })) : data);
}

export function scale(factor, data) {
  return data.map(data, (dp) => new DataPoint({ name: dp.name, value: (dp.value * factor), category: dp.category }));
}

export function filterCategory(cat) {
  return (data) => data.filter(data, (dp) => (dp.category === cat));
}

export function removeInvalid(data) {
  return data.filter(data, (dp) => (dp.value > 0.0));
}

export function computeSummary(data) {
  let total = 0.0;
  let count = 0;
  let fcount = 0.0;
  for (const dp of data) {
    total = (total + dp.value);
    count = (count + 1);
    fcount = (fcount + 1.0);
  }
  const avg = ((fcount > 0.0) ? (total / fcount) : 0.0);
  return new Summary({ count: count, total: total, average: avg });
}

export function formatSummary(s) {
  return `Items: ${s.count}, Total: ${s.total}, Average: ${s.average}`;
}

export function formatData(data) {
  let result = "";
  let first = true;
  for (const dp of data) {
    if (first) {
      result = `  ${dp.name}: ${dp.value} [${dp.category}]`;
      first = false;
    } else {
      result = (result + `
  ${dp.name}: ${dp.value} [${dp.category}]`);
    }
  }
  return result;
}

export function buildReportPipeline() {
  return (composeX) => formatSummary((composeX) => computeSummary(normalize(composeX))(composeX));
}

export function applyPipeline(title, data, pipeline) {
  const body = pipeline(data);
  return new Report({ title: title, body: body });
}

export function printReport(report) {
  console.log(`--- ${report.title} ---`);
  console.log(report.body);
  return console.log("");
}

function main() {
  const data = [new DataPoint({ name: "alpha", value: 10.0, category: "sensor" }), new DataPoint({ name: "beta", value: 25.0, category: "sensor" }), new DataPoint({ name: "gamma", value: 5.0, category: "manual" }), new DataPoint({ name: "delta", value: 40.0, category: "sensor" }), new DataPoint({ name: "epsilon", value: 15.0, category: "manual" }), new DataPoint({ name: "zeta", value: 0.0, category: "sensor" }), new DataPoint({ name: "eta", value: 30.0, category: "manual" })];
  console.log("=== Data Pipeline Demo ===");
  console.log("");
  console.log("--- Raw Data Summary ---");
  const rawSummary = formatSummary(computeSummary(data));
  console.log(rawSummary);
  console.log("");
  console.log("--- Cleaned + Normalized ---");
  const cleaned = normalize(removeInvalid(data));
  const cleanedListing = formatData(cleaned);
  console.log(cleanedListing);
  console.log("");
  const sensorFilter = filterCategory("sensor");
  const manualFilter = filterCategory("manual");
  const sensorData = sensorFilter(data);
  const manualData = manualFilter(data);
  console.log("--- Sensor Data Summary ---");
  const sensorSummary = formatSummary(computeSummary(removeInvalid(sensorData)));
  console.log(sensorSummary);
  console.log("");
  console.log("--- Manual Data Summary ---");
  const manualSummary = formatSummary(computeSummary(manualData));
  console.log(manualSummary);
  console.log("");
  const reportPipeline = buildReportPipeline();
  const fullReport = applyPipeline("Full Dataset Report", data, reportPipeline);
  printReport(fullReport);
  const sensorReport = applyPipeline("Sensor Report", sensorData, reportPipeline);
  printReport(sensorReport);
  const manualReport = applyPipeline("Manual Report", manualData, reportPipeline);
  printReport(manualReport);
  console.log("--- Scaled Sensor Summary ---");
  const scaledResult = formatSummary(computeSummary(normalize(removeInvalid(sensorData))));
  console.log(scaledResult);
  console.log("");
  return console.log("=== Pipeline Complete ===");
}
export { DataPoint, Report, Summary };
main();
//# sourceMappingURL=main.js.map
