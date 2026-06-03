from __future__ import annotations
from _bock_runtime import *
from typing import Union
from abc import ABC, abstractmethod
from dataclasses import dataclass

@dataclass(frozen=True)
class Priority_Low:
    _tag: str = "Low"

@dataclass(frozen=True)
class Priority_Medium:
    _tag: str = "Medium"

@dataclass(frozen=True)
class Priority_High:
    _tag: str = "High"

@dataclass(frozen=True)
class Priority_Critical:
    _tag: str = "Critical"
Priority = Union[Priority_Low, Priority_Medium, Priority_High, Priority_Critical]

@dataclass(frozen=True)
class Status_Pending:
    _tag: str = "Pending"

@dataclass(frozen=True)
class Status_InProgress:
    _tag: str = "InProgress"

@dataclass(frozen=True)
class Status_Done:
    _tag: str = "Done"

@dataclass(frozen=True)
class Status_Cancelled:
    _tag: str = "Cancelled"
Status = Union[Status_Pending, Status_InProgress, Status_Done, Status_Cancelled]

@dataclass
class Task:
    id: int
    title: str
    priority: Priority
    status: Status

class TaskStorage(ABC):
    @abstractmethod
    def save_task(self, task: Task) -> _BockOk | _BockErr:
        ...

    @abstractmethod
    def find_task(self, id: int) -> _BockOk | _BockErr:
        ...

    @abstractmethod
    def all_tasks(self) -> list[Task]:
        ...

class Logger(ABC):
    @abstractmethod
    def log(self, msg: str) -> None:
        ...

# composite effect ApiEffects = TaskStorage + Logger

def list_pending(tasks: list[Task]) -> list[Task]:
    return tasks.filter(lambda task: (lambda __v: True if isinstance(__v, Status_Pending) else (False))(task.status))

def list_urgent(tasks: list[Task]) -> list[Task]:
    return tasks.filter(lambda task: (lambda __v: True if isinstance(__v, Priority_High) else (True if isinstance(__v, Priority_Critical) else (False)))(task.priority))

def priority_label(p: Priority) -> str:
    return (lambda __v: "LOW" if isinstance(__v, Priority_Low) else ("MED" if isinstance(__v, Priority_Medium) else ("HIGH" if isinstance(__v, Priority_High) else ("CRIT"))))(p)

def status_label(s: Status) -> str:
    return (lambda __v: "PENDING" if isinstance(__v, Status_Pending) else ("IN_PROGRESS" if isinstance(__v, Status_InProgress) else ("DONE" if isinstance(__v, Status_Done) else ("CANCELLED"))))(s)

def format_task(task: Task) -> str:
    return f"[{priority_label(task.priority)}] {task.title} ({status_label(task.status)})"

def create_task(id: int, title: str, priority: Priority, *, task_storage: TaskStorage, logger: Logger) -> _BockOk | _BockErr:
    logger.log(f"Creating task: {title}")
    task = Task(id=id, title=title, priority=priority, status=Status_Pending())
    task_storage.save_task(task)
    logger.log(f"Task {id} created successfully")
    return _BockOk(task)

def complete_task(id: int, *, task_storage: TaskStorage, logger: Logger) -> _BockOk | _BockErr:
    logger.log(f"Completing task {id}")
    task = task_storage.find_task(id)
    completed = Task(id=task.id, title=task.title, priority=task.priority, status=Status_Done())
    task_storage.save_task(completed)
    logger.log(f"Task {id} marked as done")
    return _BockOk(completed)

def cancel_task(id: int, *, task_storage: TaskStorage, logger: Logger) -> _BockOk | _BockErr:
    logger.log(f"Cancelling task {id}")
    task = task_storage.find_task(id)
    cancelled = Task(id=task.id, title=task.title, priority=task.priority, status=Status_Cancelled())
    task_storage.save_task(cancelled)
    logger.log(f"Task {id} cancelled")
    return _BockOk(cancelled)

def get_pending_tasks(*, task_storage: TaskStorage) -> list[Task]:
    all = task_storage.all_tasks()
    return list_pending(all)

def get_urgent_tasks(*, task_storage: TaskStorage) -> list[Task]:
    all = task_storage.all_tasks()
    return list_urgent(all)

@dataclass
class MemoryStore(TaskStorage):
    tasks: list[Task]

    def save_task(self, task: Task) -> _BockOk | _BockErr:
        return _BockOk(None)

    def find_task(self, id: int) -> _BockOk | _BockErr:
        return _BockErr("not found")

    def all_tasks(self) -> list[Task]:
        return []

class ConsoleLogger(Logger):

    def log(self, msg: str) -> None:
        return print(f"[LOG] {msg}")

def run_demo(*, task_storage: TaskStorage, logger: Logger) -> None:
    print("=== Task API Demo ===")
    print("")
    result1 = create_task(1, "Design API schema", Priority_High(), task_storage=task_storage, logger=logger)
    match result1:
        case _BockOk(task):
            return print(f"Created: {format_task(task)}")
        case _BockErr(e):
            return print(f"Error: {e}")
    result2 = create_task(2, "Write unit tests", Priority_Medium(), task_storage=task_storage, logger=logger)
    match result2:
        case _BockOk(task):
            return print(f"Created: {format_task(task)}")
        case _BockErr(e):
            return print(f"Error: {e}")
    result3 = create_task(3, "Deploy to staging", Priority_Critical(), task_storage=task_storage, logger=logger)
    match result3:
        case _BockOk(task):
            return print(f"Created: {format_task(task)}")
        case _BockErr(e):
            return print(f"Error: {e}")
    result4 = create_task(4, "Update changelog", Priority_Low(), task_storage=task_storage, logger=logger)
    match result4:
        case _BockOk(task):
            return print(f"Created: {format_task(task)}")
        case _BockErr(e):
            return print(f"Error: {e}")
    print("")
    completed = complete_task(1, task_storage=task_storage, logger=logger)
    match completed:
        case _BockOk(task):
            return print(f"Completed: {format_task(task)}")
        case _BockErr(e):
            return print(f"Could not complete: {e}")
    cancelled = cancel_task(4, task_storage=task_storage, logger=logger)
    match cancelled:
        case _BockOk(task):
            return print(f"Cancelled: {format_task(task)}")
        case _BockErr(e):
            return print(f"Could not cancel: {e}")
    print("")
    pending = get_pending_tasks(task_storage=task_storage)
    print(f"=== Pending Tasks ({len(pending)}) ===")
    for task in pending:
        return print(f"  {format_task(task)}")
    urgent = get_urgent_tasks(task_storage=task_storage)
    print("")
    print(f"=== Urgent Tasks ({len(urgent)}) ===")
    for task in urgent:
        return print(f"  {format_task(task)}")
    print("")
    return print("=== Demo Complete ===")

def main():
    store = MemoryStore(tasks=[])
    logger = ConsoleLogger()
    __task_storage_h1: TaskStorage = store
    __logger_h1: Logger = logger
    run_demo(task_storage=__task_storage_h1, logger=__logger_h1)
if __name__ == "__main__":
    main()
