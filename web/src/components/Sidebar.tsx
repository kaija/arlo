import { useEffect, useRef, useState } from "react";
import type { TaskEntry, TaskStatus, TodoItem } from "../types";

interface Props {
  tasks: TaskEntry[];
  todos: TodoItem[];
}

const TASK_GROUPS: TaskStatus[] = ["Running", "Pending", "Completed", "Failed", "Killed"];

function TasksTab({ tasks }: { tasks: TaskEntry[] }) {
  if (tasks.length === 0) return <p className="sidebar__empty">No background tasks yet.</p>;
  return (
    <div className="sidebar__list">
      {TASK_GROUPS.map((status) => {
        const group = tasks.filter((t) => t.status === status);
        if (group.length === 0) return null;
        return (
          <div key={status} className="sidebar__group">
            <h3>
              {status} ({group.length})
            </h3>
            {group.map((task) => (
              <div key={task.id} className={`task-row task-row--${task.status.toLowerCase()}`}>
                <div className="task-row__description">{task.description}</div>
                {task.output && <div className="task-row__detail">{task.output}</div>}
                {task.last_error && <div className="task-row__detail task-row__detail--error">{task.last_error}</div>}
              </div>
            ))}
          </div>
        );
      })}
    </div>
  );
}

function TodosTab({ todos }: { todos: TodoItem[] }) {
  if (todos.length === 0) return <p className="sidebar__empty">No todo items yet.</p>;
  const icon = { Pending: "☐", InProgress: "◐", Completed: "☑" } as const;
  return (
    <div className="sidebar__list">
      {todos.map((todo) => (
        <div key={todo.id} className={`todo-row todo-row--${todo.status.toLowerCase()}`}>
          <span className="todo-row__icon">{icon[todo.status]}</span>
          {todo.status === "InProgress" && todo.active_form ? todo.active_form : todo.content}
        </div>
      ))}
    </div>
  );
}

export function Sidebar({ tasks, todos }: Props) {
  const [active, setActive] = useState<"tasks" | "todos">("tasks");
  const [unreadTasks, setUnreadTasks] = useState(false);
  const terminalCount = tasks.filter((t) => t.status === "Completed" || t.status === "Failed").length;
  const prevTerminalCount = useRef(terminalCount);

  useEffect(() => {
    if (terminalCount !== prevTerminalCount.current && active !== "tasks") {
      setUnreadTasks(true);
    }
    prevTerminalCount.current = terminalCount;
  }, [terminalCount, active]);

  return (
    <aside className="sidebar">
      <div className="sidebar__tabs">
        <button
          type="button"
          className={active === "tasks" ? "sidebar__tab sidebar__tab--active" : "sidebar__tab"}
          onClick={() => {
            setActive("tasks");
            setUnreadTasks(false);
          }}
        >
          Tasks{unreadTasks && <span className="sidebar__unread-dot" />}
        </button>
        <button
          type="button"
          className={active === "todos" ? "sidebar__tab sidebar__tab--active" : "sidebar__tab"}
          onClick={() => setActive("todos")}
        >
          Todos
        </button>
      </div>
      {active === "tasks" ? <TasksTab tasks={tasks} /> : <TodosTab todos={todos} />}
    </aside>
  );
}
