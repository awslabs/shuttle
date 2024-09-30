import { HierarchyEntry } from "../common/hierarchy.mts";
import { Object } from "../common/objects.mts";
import { Task } from "../common/tasks.mts";

export interface HierarchyEntryUI {
    entry: HierarchyEntry,

    isTask: boolean,

    /**
     * Is this entry currently visible? An entry may be hidden if its parent
     * is collapsed.
     */
    visible: boolean,

    /**
     * Index of this entry among the currently visible entries. Collapsed
     * entries have the same visible index as the last visible entry.
     */
    visibleIndex: number,

    /**
     * Is the entry open (`true`) or collapsed (`false`)?
     */
    open: boolean,
};

export type ObjectUI = HierarchyEntryUI & {
    isTask: false,
    entry: Object,
};

export type TaskUI = HierarchyEntryUI & {
    isTask: true,
    entry: Task,
};

export function createObjectUI(object: Object): ObjectUI {
    return {
        entry: object,
        isTask: false,
        visible: true,
        visibleIndex: 0,
        open: true,
    };
}

export function createTaskUI(task: Task): TaskUI {
    return {
        entry: task,
        isTask: true,
        visible: true,
        visibleIndex: 0,
        open: true,
    };
}
