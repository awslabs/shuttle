import { ExtContextData } from "./context.mts";
import { HierarchyEntry } from "./hierarchy.mts";
import { ScheduleTask } from "./schedule.mts";

export type TaskId = number;

export type Task = HierarchyEntry & {
    source: ScheduleTask,
    id: TaskId,
    isTask: true,
    isFuture: boolean,
    blockedAt: number[], // TODO: blockedRanges: [], ?
};

/**
 * Reads the given task from the annotated schedule.
 */
export function readTask(ctx: ExtContextData, taskId: number): Task {
    const {created_by, first_step, name} = ctx.source.tasks[taskId];
    const task: Task = {
        source: ctx.source.tasks[taskId],
        id: taskId,
        name,
        isTask: true,
        isFuture: false,
        createEvent: null,
        createdBy: created_by,
        createdAt: first_step,
        firstStep: first_step > 10000000 ? 0 : first_step, // DEMO HACK: sometimes -1 is serialised here ...
        lastStep: ctx.source.events.length - 1, // last_step,
        children: [],
        path: [0],
        flatIndex: 0,
        depth: 0,
        events: [],
        blockedAt: [], // TODO: blockedRanges: [], ?
    };

    // `created_by === taskId` only for the initial/main thread. Otherwise,
    // this task was created by another.
    if (created_by !== taskId) {
        ctx.tasks[created_by].children.push(task);
        task.path = ctx.tasks[created_by].path.concat([taskId]);
    }

    return task;
}

export function writeTask(task: Task): ScheduleTask {
    return {
        created_by: task.createdBy,
        first_step: task.firstStep,
        last_step: task.source.last_step,
        name: task.name,
    };
}
