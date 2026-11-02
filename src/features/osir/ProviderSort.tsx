import type { ComponentProps, ReactNode } from "react";
import {
  DndContext,
  closestCenter,
  KeyboardSensor,
  PointerSensor,
  useSensor,
  useSensors,
} from "@dnd-kit/core";
import {
  SortableContext,
  sortableKeyboardCoordinates,
  useSortable,
  verticalListSortingStrategy,
} from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { GripVertical } from "lucide-react";

export function ProviderSortList({
  ids,
  disabled,
  onMove,
  children,
}: {
  ids: string[];
  disabled: boolean;
  onMove: (from: string, to: string) => void;
  children: ReactNode;
}) {
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 8 } }),
    useSensor(KeyboardSensor, {
      coordinateGetter: sortableKeyboardCoordinates,
    }),
  );
  return (
    <DndContext
      sensors={sensors}
      collisionDetection={closestCenter}
      accessibility={{
        screenReaderInstructions: {
          draggable:
            "按空格开始排序，使用上下方向键调整位置，再按空格保存，或按 Esc 取消。",
        },
        announcements: {
          onDragStart: () => "开始调整供应商顺序。",
          onDragOver: ({ over }) =>
            over
              ? `移动到第 ${ids.indexOf(String(over.id)) + 1} 位。`
              : undefined,
          onDragEnd: ({ over }) => (over ? "已放下供应商。" : "已取消排序。"),
          onDragCancel: () => "已取消排序。",
        },
      }}
      onDragEnd={({ active, over }) => {
        if (!disabled && over && active.id !== over.id)
          onMove(String(active.id), String(over.id));
      }}
    >
      <SortableContext items={ids} strategy={verticalListSortingStrategy}>
        {children}
      </SortableContext>
    </DndContext>
  );
}

export function SortableProviderCard({
  providerId,
  name,
  sortable,
  disabled,
  children,
  className,
  ...props
}: ComponentProps<"article"> & {
  providerId: string;
  name: string;
  sortable: boolean;
  disabled: boolean;
}) {
  const {
    attributes,
    listeners,
    setNodeRef,
    setActivatorNodeRef,
    transform,
    transition,
    isDragging,
  } = useSortable({ id: providerId, disabled: disabled || !sortable });
  return (
    <article
      {...props}
      ref={setNodeRef}
      className={`${className} ${sortable ? "osir-provider-sortable" : ""}`}
      style={{
        transform: CSS.Transform.toString(transform),
        transition,
        zIndex: isDragging ? 1 : undefined,
      }}
    >
      {sortable && (
        <button
          type="button"
          ref={setActivatorNodeRef}
          className="osir-provider-drag"
          disabled={disabled}
          {...attributes}
          {...listeners}
          aria-label={`调整 ${name} 的顺序`}
          title="拖动调整顺序；也可按空格后使用方向键"
        >
          <GripVertical size={12} aria-hidden="true" />
        </button>
      )}
      {children}
    </article>
  );
}
