#![allow(
    unused_variables,
    unused_imports,
    unused_parens,
    dead_code,
    non_upper_case_globals
)]

#[derive(Clone)]
pub enum Priority {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Clone)]
pub enum Status {
    Pending,
    InProgress,
    Done,
    Cancelled,
}

#[derive(Clone)]
pub struct Task {
    pub id: i64,
    pub title: String,
    pub priority: Priority,
    pub status: Status,
}

pub trait TaskStorage {
    fn save_task(&self, task: Task) -> Result<(), String>;

    fn find_task(&self, id: i64) -> Result<Task, String>;

    fn all_tasks(&self) -> Vec<Task>;
}

pub trait Logger {
    fn log(&self, msg: String) -> ();
}

// composite effect ApiEffects = TaskStorage + Logger

pub fn list_pending(tasks: Vec<Task>) -> Vec<Task> {
    tasks.filter(|task: _| match task.status {
        Status::Pending => true,
        _ => false,
    })
}

pub fn list_urgent(tasks: Vec<Task>) -> Vec<Task> {
    tasks.filter(|task: _| match task.priority {
        Priority::High => true,
        Priority::Critical => true,
        _ => false,
    })
}

pub fn priority_label(p: Priority) -> String {
    match p {
        Priority::Low => "LOW".to_string(),
        Priority::Medium => "MED".to_string(),
        Priority::High => "HIGH".to_string(),
        Priority::Critical => "CRIT".to_string(),
    }
}

pub fn status_label(s: Status) -> String {
    match s {
        Status::Pending => "PENDING".to_string(),
        Status::InProgress => "IN_PROGRESS".to_string(),
        Status::Done => "DONE".to_string(),
        Status::Cancelled => "CANCELLED".to_string(),
    }
}

pub fn format_task(task: Task) -> String {
    format!("[{}] {} ({})", priority_label(task.priority), task.title, status_label(task.status))
}

pub fn create_task(id: i64, title: String, priority: Priority, task_storage: &impl TaskStorage, logger: &impl Logger) -> Result<Task, String> {
    logger.log(format!("Creating task: {}", title));
    let task = Task { id: id, title: title, priority: priority, status: Status::Pending };
    task_storage.save_task(task)?;
    logger.log(format!("Task {} created successfully", id));
    Ok(task)
}

pub fn complete_task(id: i64, task_storage: &impl TaskStorage, logger: &impl Logger) -> Result<Task, String> {
    logger.log(format!("Completing task {}", id));
    let task = task_storage.find_task(id)?;
    let completed = Task { id: task.id, title: task.title, priority: task.priority, status: Status::Done };
    task_storage.save_task(completed)?;
    logger.log(format!("Task {} marked as done", id));
    Ok(completed)
}

pub fn cancel_task(id: i64, task_storage: &impl TaskStorage, logger: &impl Logger) -> Result<Task, String> {
    logger.log(format!("Cancelling task {}", id));
    let task = task_storage.find_task(id)?;
    let cancelled = Task { id: task.id, title: task.title, priority: task.priority, status: Status::Cancelled };
    task_storage.save_task(cancelled)?;
    logger.log(format!("Task {} cancelled", id));
    Ok(cancelled)
}

pub fn get_pending_tasks(task_storage: &impl TaskStorage) -> Vec<Task> {
    let all = task_storage.all_tasks();
    list_pending(all)
}

pub fn get_urgent_tasks(task_storage: &impl TaskStorage) -> Vec<Task> {
    let all = task_storage.all_tasks();
    list_urgent(all)
}

#[derive(Clone)]
pub struct MemoryStore {
    pub tasks: Vec<Task>,
}

impl TaskStorage for MemoryStore {
    fn save_task(&self, task: Task) -> Result<(), String> {
        Ok(())
    }

    fn find_task(&self, id: i64) -> Result<Task, String> {
        Err("not found".to_string())
    }

    fn all_tasks(&self) -> Vec<Task> {
        vec![]
    }
}

#[derive(Clone)]
pub struct ConsoleLogger {
}

impl Logger for ConsoleLogger {
    fn log(&self, msg: String) -> () {
        println!("{}", format!("[LOG] {}", msg))
    }
}

pub fn run_demo(task_storage: &impl TaskStorage, logger: &impl Logger) -> () {
    println!("{}", "=== Task API Demo ===".to_string());
    println!("{}", "".to_string());
    let result1 = create_task(1_i64, "Design API schema".to_string(), Priority::High, &task_storage, &logger);
    match result1 {
        Ok(task) => println!("{}", format!("Created: {}", format_task(task))),
        Err(e) => println!("{}", format!("Error: {}", e)),
    }
    let result2 = create_task(2_i64, "Write unit tests".to_string(), Priority::Medium, &task_storage, &logger);
    match result2 {
        Ok(task) => println!("{}", format!("Created: {}", format_task(task))),
        Err(e) => println!("{}", format!("Error: {}", e)),
    }
    let result3 = create_task(3_i64, "Deploy to staging".to_string(), Priority::Critical, &task_storage, &logger);
    match result3 {
        Ok(task) => println!("{}", format!("Created: {}", format_task(task))),
        Err(e) => println!("{}", format!("Error: {}", e)),
    }
    let result4 = create_task(4_i64, "Update changelog".to_string(), Priority::Low, &task_storage, &logger);
    match result4 {
        Ok(task) => println!("{}", format!("Created: {}", format_task(task))),
        Err(e) => println!("{}", format!("Error: {}", e)),
    }
    println!("{}", "".to_string());
    let completed = complete_task(1_i64, &task_storage, &logger);
    match completed {
        Ok(task) => println!("{}", format!("Completed: {}", format_task(task))),
        Err(e) => println!("{}", format!("Could not complete: {}", e)),
    }
    let cancelled = cancel_task(4_i64, &task_storage, &logger);
    match cancelled {
        Ok(task) => println!("{}", format!("Cancelled: {}", format_task(task))),
        Err(e) => println!("{}", format!("Could not cancel: {}", e)),
    }
    println!("{}", "".to_string());
    let pending = get_pending_tasks(&task_storage);
    println!("{}", format!("=== Pending Tasks ({}) ===", ((pending).len() as i64)));
    for task in pending {
        println!("{}", format!("  {}", format_task(task)))
    }
    let urgent = get_urgent_tasks(&task_storage);
    println!("{}", "".to_string());
    println!("{}", format!("=== Urgent Tasks ({}) ===", ((urgent).len() as i64)));
    for task in urgent {
        println!("{}", format!("  {}", format_task(task)))
    }
    println!("{}", "".to_string());
    println!("{}", "=== Demo Complete ===".to_string())
}

fn main() {
    let store = MemoryStore { tasks: vec![] };
    let logger = ConsoleLogger {  };
    {
        let __task_storage = store;
        let __logger = logger;
        run_demo(&__task_storage, &__logger)
    }
}
