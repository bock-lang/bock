import type { BockResult } from "./_bock_runtime.js";
export type Priority = Priority_Low | Priority_Medium | Priority_High | Priority_Critical;

interface Priority_Low { readonly _tag: "Low"; }
const Priority_Low: Priority_Low = Object.freeze({ _tag: "Low" as const });
interface Priority_Medium { readonly _tag: "Medium"; }
const Priority_Medium: Priority_Medium = Object.freeze({ _tag: "Medium" as const });
interface Priority_High { readonly _tag: "High"; }
const Priority_High: Priority_High = Object.freeze({ _tag: "High" as const });
interface Priority_Critical { readonly _tag: "Critical"; }
const Priority_Critical: Priority_Critical = Object.freeze({ _tag: "Critical" as const });

export type Status = Status_Pending | Status_InProgress | Status_Done | Status_Cancelled;

interface Status_Pending { readonly _tag: "Pending"; }
const Status_Pending: Status_Pending = Object.freeze({ _tag: "Pending" as const });
interface Status_InProgress { readonly _tag: "InProgress"; }
const Status_InProgress: Status_InProgress = Object.freeze({ _tag: "InProgress" as const });
interface Status_Done { readonly _tag: "Done"; }
const Status_Done: Status_Done = Object.freeze({ _tag: "Done" as const });
interface Status_Cancelled { readonly _tag: "Cancelled"; }
const Status_Cancelled: Status_Cancelled = Object.freeze({ _tag: "Cancelled" as const });

export class Task {
  id: number;
  title: string;
  priority: Priority;
  status: Status;
  constructor({ id, title, priority, status }: { id: number; title: string; priority: Priority; status: Status }) {
    this.id = id;
    this.title = title;
    this.priority = priority;
    this.status = status;
  }
}

export interface TaskStorage {
  save_task(task: Task): BockResult<void, string>;
  find_task(id: number): BockResult<Task, string>;
  all_tasks(): Array<Task>;
}

export interface Logger {
  log(msg: string): void;
}

// composite effect ApiEffects = TaskStorage + Logger

export function listPending(tasks: Array<Task>): Array<Task> {
  return tasks.filter(tasks, (task) => (() => {
    const __match2 = task.status;
    switch (__match2._tag) {
      case "Pending": {
        return true;
        break;
      }
      default: {
        return false;
        break;
      }
    }
  })());
}

export function listUrgent(tasks: Array<Task>): Array<Task> {
  return tasks.filter(tasks, (task) => (() => {
    const __match4 = task.priority;
    switch (__match4._tag) {
      case "High": {
        return true;
        break;
      }
      case "Critical": {
        return true;
        break;
      }
      default: {
        return false;
        break;
      }
    }
  })());
}

export function priorityLabel(p: Priority): string {
  return (() => {
    switch (p._tag) {
      case "Low": {
        return "LOW";
        break;
      }
      case "Medium": {
        return "MED";
        break;
      }
      case "High": {
        return "HIGH";
        break;
      }
      case "Critical": {
        return "CRIT";
        break;
      }
    }
  })();
}

export function statusLabel(s: Status): string {
  return (() => {
    switch (s._tag) {
      case "Pending": {
        return "PENDING";
        break;
      }
      case "InProgress": {
        return "IN_PROGRESS";
        break;
      }
      case "Done": {
        return "DONE";
        break;
      }
      case "Cancelled": {
        return "CANCELLED";
        break;
      }
    }
  })();
}

export function formatTask(task: Task): string {
  return `[${priorityLabel(task.priority)}] ${task.title} (${statusLabel(task.status)})`;
}

export function createTask(id: number, title: string, priority: Priority, { taskStorage, logger }: { taskStorage: TaskStorage, logger: Logger }): BockResult<Task, string> {
  logger.log(`Creating task: ${title}`);
  const task = new Task({ id: id, title: title, priority: priority, status: Status_Pending });
  taskStorage.save_task(task);
  logger.log(`Task ${id} created successfully`);
  return { _tag: "Ok" as const, _0: task };
}

export function completeTask(id: number, { taskStorage, logger }: { taskStorage: TaskStorage, logger: Logger }): BockResult<Task, string> {
  logger.log(`Completing task ${id}`);
  const task = taskStorage.find_task(id);
  const completed = new Task({ id: task.id, title: task.title, priority: task.priority, status: Status_Done });
  taskStorage.save_task(completed);
  logger.log(`Task ${id} marked as done`);
  return { _tag: "Ok" as const, _0: completed };
}

export function cancelTask(id: number, { taskStorage, logger }: { taskStorage: TaskStorage, logger: Logger }): BockResult<Task, string> {
  logger.log(`Cancelling task ${id}`);
  const task = taskStorage.find_task(id);
  const cancelled = new Task({ id: task.id, title: task.title, priority: task.priority, status: Status_Cancelled });
  taskStorage.save_task(cancelled);
  logger.log(`Task ${id} cancelled`);
  return { _tag: "Ok" as const, _0: cancelled };
}

export function getPendingTasks({ taskStorage }: { taskStorage: TaskStorage }): Array<Task> {
  const all = taskStorage.all_tasks();
  return listPending(all);
}

export function getUrgentTasks({ taskStorage }: { taskStorage: TaskStorage }): Array<Task> {
  const all = taskStorage.all_tasks();
  return listUrgent(all);
}

export class MemoryStore {
  tasks: Array<Task>;
  constructor({ tasks }: { tasks: Array<Task> }) {
    this.tasks = tasks;
  }
}

export interface MemoryStore extends TaskStorage {
  save_task(task: Task): BockResult<void, string>;
  find_task(id: number): BockResult<Task, string>;
  all_tasks(): Array<Task>;
}
// impl TaskStorage for MemoryStore
MemoryStore.prototype.save_task = function(task: Task): BockResult<void, string> {
  return { _tag: "Ok" as const, _0: undefined };
};
MemoryStore.prototype.find_task = function(id: number): BockResult<Task, string> {
  return { _tag: "Err" as const, _0: "not found" };
};
MemoryStore.prototype.all_tasks = function(): Array<Task> {
  return [];
};

export class ConsoleLogger {}

export interface ConsoleLogger extends Logger {
  log(msg: string): void;
}
// impl Logger for ConsoleLogger
ConsoleLogger.prototype.log = function(msg: string): void {
  return console.log(`[LOG] ${msg}`);
};

export function runDemo({ taskStorage, logger }: { taskStorage: TaskStorage, logger: Logger }): void {
  console.log("=== Task API Demo ===");
  console.log("");
  const result1 = createTask(1, "Design API schema", Priority_High, { taskStorage: taskStorage, logger: logger });
  switch (result1._tag) {
    case "Ok": {
      const task = result1._0;
      return console.log(`Created: ${formatTask(task)}`);
      break;
    }
    case "Err": {
      const e = result1._0;
      return console.log(`Error: ${e}`);
      break;
    }
  }
  const result2 = createTask(2, "Write unit tests", Priority_Medium, { taskStorage: taskStorage, logger: logger });
  switch (result2._tag) {
    case "Ok": {
      const task = result2._0;
      return console.log(`Created: ${formatTask(task)}`);
      break;
    }
    case "Err": {
      const e = result2._0;
      return console.log(`Error: ${e}`);
      break;
    }
  }
  const result3 = createTask(3, "Deploy to staging", Priority_Critical, { taskStorage: taskStorage, logger: logger });
  switch (result3._tag) {
    case "Ok": {
      const task = result3._0;
      return console.log(`Created: ${formatTask(task)}`);
      break;
    }
    case "Err": {
      const e = result3._0;
      return console.log(`Error: ${e}`);
      break;
    }
  }
  const result4 = createTask(4, "Update changelog", Priority_Low, { taskStorage: taskStorage, logger: logger });
  switch (result4._tag) {
    case "Ok": {
      const task = result4._0;
      return console.log(`Created: ${formatTask(task)}`);
      break;
    }
    case "Err": {
      const e = result4._0;
      return console.log(`Error: ${e}`);
      break;
    }
  }
  console.log("");
  const completed = completeTask(1, { taskStorage: taskStorage, logger: logger });
  switch (completed._tag) {
    case "Ok": {
      const task = completed._0;
      return console.log(`Completed: ${formatTask(task)}`);
      break;
    }
    case "Err": {
      const e = completed._0;
      return console.log(`Could not complete: ${e}`);
      break;
    }
  }
  const cancelled = cancelTask(4, { taskStorage: taskStorage, logger: logger });
  switch (cancelled._tag) {
    case "Ok": {
      const task = cancelled._0;
      return console.log(`Cancelled: ${formatTask(task)}`);
      break;
    }
    case "Err": {
      const e = cancelled._0;
      return console.log(`Could not cancel: ${e}`);
      break;
    }
  }
  console.log("");
  const pending = getPendingTasks({ taskStorage: taskStorage });
  console.log(`=== Pending Tasks (${(pending).length}) ===`);
  for (const task of pending) {
    return console.log(`  ${formatTask(task)}`);
  }
  const urgent = getUrgentTasks({ taskStorage: taskStorage });
  console.log("");
  console.log(`=== Urgent Tasks (${(urgent).length}) ===`);
  for (const task of urgent) {
    return console.log(`  ${formatTask(task)}`);
  }
  console.log("");
  return console.log("=== Demo Complete ===");
}

function main() {
  const store = new MemoryStore({ tasks: [] });
  const logger = new ConsoleLogger();
  {
    const __taskStorage: TaskStorage = store;
    const __logger: Logger = logger;
    runDemo({ taskStorage: __taskStorage, logger: __logger });
  }
}
export { Priority_Critical, Priority_High, Priority_Low, Priority_Medium, Status_Cancelled, Status_Done, Status_InProgress, Status_Pending };
main();
//# sourceMappingURL=main.ts.map
