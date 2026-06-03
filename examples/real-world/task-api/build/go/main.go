package main

import "fmt"

type Priority interface {
	isPriority()
}

type PriorityLow struct{}

func (PriorityLow) isPriority() {}

type PriorityMedium struct{}

func (PriorityMedium) isPriority() {}

type PriorityHigh struct{}

func (PriorityHigh) isPriority() {}

type PriorityCritical struct{}

func (PriorityCritical) isPriority() {}

type Status interface {
	isStatus()
}

type StatusPending struct{}

func (StatusPending) isStatus() {}

type StatusInProgress struct{}

func (StatusInProgress) isStatus() {}

type StatusDone struct{}

func (StatusDone) isStatus() {}

type StatusCancelled struct{}

func (StatusCancelled) isStatus() {}

type Task struct {
	Id	int64
	Title	string
	Priority	Priority
	Status	Status
}

type TaskStorage interface {
	SaveTask(Task) __bockResult
	FindTask(int64) __bockResult
	AllTasks() []Task
}

type Logger interface {
	Log(string)
}

// composite effect ApiEffects = TaskStorage + Logger

func ListPending(tasks []Task) []Task {
	return tasks.filter(func(task interface{}) interface{} { return func() []Task { switch task.Status.(type) { case StatusPending: return true; default: return false; }; panic("unreachable") }() })
}

func ListUrgent(tasks []Task) []Task {
	return tasks.filter(func(task interface{}) interface{} { return func() []Task { switch task.Priority.(type) { case PriorityHigh: return true; case PriorityCritical: return true; default: return false; }; panic("unreachable") }() })
}

func PriorityLabel(p Priority) string {
	return func() string { switch p.(type) { case PriorityLow: return "LOW"; case PriorityMedium: return "MED"; case PriorityHigh: return "HIGH"; case PriorityCritical: return "CRIT"; }; panic("unreachable") }()
}

func StatusLabel(s Status) string {
	return func() string { switch s.(type) { case StatusPending: return "PENDING"; case StatusInProgress: return "IN_PROGRESS"; case StatusDone: return "DONE"; case StatusCancelled: return "CANCELLED"; }; panic("unreachable") }()
}

func FormatTask(task Task) string {
	return fmt.Sprintf("[%v] %v (%v)", PriorityLabel(task.Priority), task.Title, StatusLabel(task.Status))
}

func CreateTask(id int64, title string, priority Priority, taskStorage TaskStorage, logger Logger) __bockResult {
	logger.Log(fmt.Sprintf("Creating task: %v", title))
	task := Task{Id: id, Title: title, Priority: priority, Status: StatusPending{}}
	taskStorage.SaveTask(task)
	logger.Log(fmt.Sprintf("Task %v created successfully", id))
	return __bockOk(task)
}

func CompleteTask(id int64, taskStorage TaskStorage, logger Logger) __bockResult {
	logger.Log(fmt.Sprintf("Completing task %v", id))
	task := taskStorage.FindTask(id)
	completed := Task{Id: task.Id, Title: task.Title, Priority: task.Priority, Status: StatusDone{}}
	taskStorage.SaveTask(completed)
	logger.Log(fmt.Sprintf("Task %v marked as done", id))
	return __bockOk(completed)
}

func CancelTask(id int64, taskStorage TaskStorage, logger Logger) __bockResult {
	logger.Log(fmt.Sprintf("Cancelling task %v", id))
	task := taskStorage.FindTask(id)
	cancelled := Task{Id: task.Id, Title: task.Title, Priority: task.Priority, Status: StatusCancelled{}}
	taskStorage.SaveTask(cancelled)
	logger.Log(fmt.Sprintf("Task %v cancelled", id))
	return __bockOk(cancelled)
}

func GetPendingTasks(taskStorage TaskStorage) []Task {
	all := taskStorage.AllTasks()
	return ListPending(all)
}

func GetUrgentTasks(taskStorage TaskStorage) []Task {
	all := taskStorage.AllTasks()
	return ListUrgent(all)
}

type MemoryStore struct {
	Tasks	[]Task
}

func (m MemoryStore) SaveTask(task Task) __bockResult {
	return __bockOk(nil)
}

func (m MemoryStore) FindTask(id int64) __bockResult {
	return __bockErr("not found")
}

func (m MemoryStore) AllTasks() []Task {
	return []Task{}
}

type ConsoleLogger struct {
}

func (c ConsoleLogger) Log(msg string) {
	fmt.Println(fmt.Sprintf("[LOG] %v", msg))
}

func RunDemo(taskStorage TaskStorage, logger Logger) {
	fmt.Println("=== Task API Demo ===")
	fmt.Println("")
	result1 := CreateTask(1, "Design API schema", PriorityHigh{}, taskStorage, logger)
	__res := result1
	if __res.tag == "Ok" { task := __res.v; _ = task; 
		fmt.Println(fmt.Sprintf("Created: %v", FormatTask(task)))
	} else { e := __res.v; _ = e; 
		fmt.Println(fmt.Sprintf("Error: %v", e))
	}
	result2 := CreateTask(2, "Write unit tests", PriorityMedium{}, taskStorage, logger)
	__res := result2
	if __res.tag == "Ok" { task := __res.v; _ = task; 
		fmt.Println(fmt.Sprintf("Created: %v", FormatTask(task)))
	} else { e := __res.v; _ = e; 
		fmt.Println(fmt.Sprintf("Error: %v", e))
	}
	result3 := CreateTask(3, "Deploy to staging", PriorityCritical{}, taskStorage, logger)
	__res := result3
	if __res.tag == "Ok" { task := __res.v; _ = task; 
		fmt.Println(fmt.Sprintf("Created: %v", FormatTask(task)))
	} else { e := __res.v; _ = e; 
		fmt.Println(fmt.Sprintf("Error: %v", e))
	}
	result4 := CreateTask(4, "Update changelog", PriorityLow{}, taskStorage, logger)
	__res := result4
	if __res.tag == "Ok" { task := __res.v; _ = task; 
		fmt.Println(fmt.Sprintf("Created: %v", FormatTask(task)))
	} else { e := __res.v; _ = e; 
		fmt.Println(fmt.Sprintf("Error: %v", e))
	}
	fmt.Println("")
	completed := CompleteTask(1, taskStorage, logger)
	__res := completed
	if __res.tag == "Ok" { task := __res.v; _ = task; 
		fmt.Println(fmt.Sprintf("Completed: %v", FormatTask(task)))
	} else { e := __res.v; _ = e; 
		fmt.Println(fmt.Sprintf("Could not complete: %v", e))
	}
	cancelled := CancelTask(4, taskStorage, logger)
	__res := cancelled
	if __res.tag == "Ok" { task := __res.v; _ = task; 
		fmt.Println(fmt.Sprintf("Cancelled: %v", FormatTask(task)))
	} else { e := __res.v; _ = e; 
		fmt.Println(fmt.Sprintf("Could not cancel: %v", e))
	}
	fmt.Println("")
	pending := GetPendingTasks(taskStorage)
	fmt.Println(fmt.Sprintf("=== Pending Tasks (%v) ===", int64(len(pending))))
	for _, task := range pending {
		fmt.Println(fmt.Sprintf("  %v", FormatTask(task)))
	}
	urgent := GetUrgentTasks(taskStorage)
	fmt.Println("")
	fmt.Println(fmt.Sprintf("=== Urgent Tasks (%v) ===", int64(len(urgent))))
	for _, task := range urgent {
		fmt.Println(fmt.Sprintf("  %v", FormatTask(task)))
	}
	fmt.Println("")
	fmt.Println("=== Demo Complete ===")
}

func main() {
	store := MemoryStore{Tasks: []Task{}}
	logger := ConsoleLogger{}
	{
		__taskStorage := store
		__logger := logger
		_ = __taskStorage
		_ = __logger
		RunDemo(__taskStorage, __logger)
	}
}
