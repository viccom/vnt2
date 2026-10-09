<script setup>
import { ref, watch, onMounted, onBeforeUnmount } from "vue";
import { useNotificationStore } from "../stores/notifications";

const store = useNotificationStore();
const open = ref(false);
const rootRef = ref(null);

const toggle = () => {
  open.value = !open.value;
  // 展开即已读：角标清零，条目仍在面板内保留展示
  if (open.value) store.markSeen();
};

// 面板保持打开时，轮询新到的条目已展示在用户眼前，同步标已读
watch(
  () => store.list,
  () => {
    if (open.value) store.markSeen();
  },
  { deep: true },
);

const onDocClick = (event) => {
  if (open.value && rootRef.value && !rootRef.value.contains(event.target)) {
    open.value = false;
  }
};

onMounted(() => {
  store.start();
  document.addEventListener("click", onDocClick);
});
onBeforeUnmount(() => {
  store.stop();
  document.removeEventListener("click", onDocClick);
});

const levelClass = (level) =>
  level === "important"
    ? "bg-red-100 text-red-700 dark:bg-red-500/15 dark:text-red-300"
    : level === "warning"
      ? "bg-amber-100 text-amber-700 dark:bg-amber-500/15 dark:text-amber-300"
      : "bg-slate-100 text-slate-600 dark:bg-slate-700/60 dark:text-slate-300";
const levelLabel = (level) =>
  level === "important" ? "重要" : level === "warning" ? "警告" : "通知";

// 展示用本地时间 MM-DD HH:mm（通知为会话级，不含跨年场景）
const formatTime = (ms) => {
  const d = new Date(ms);
  const pad = (x) => String(x).padStart(2, "0");
  return `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
};
</script>

<template>
  <div ref="rootRef" class="relative">
    <button
      class="relative grid h-8 w-8 place-items-center rounded-lg border border-slate-200 bg-white text-slate-500 hover:border-indigo-300 hover:text-indigo-600 dark:border-slate-700 dark:bg-slate-800 dark:text-slate-400 dark:hover:border-indigo-500 dark:hover:text-indigo-400"
      :title="open ? '关闭平台通知' : '平台通知'"
      :aria-label="open ? '关闭平台通知' : `平台通知，${store.unread} 条未读`"
      @click="toggle"
    >
      <svg class="h-4 w-4 fill-none stroke-current" viewBox="0 0 24 24">
        <path
          d="M6 9a6 6 0 1 1 12 0c0 4 1.5 5.5 2 6H4c.5-.5 2-2 2-6Zm4.5 9a1.5 1.5 0 0 0 3 0"
          stroke-linecap="round"
          stroke-linejoin="round"
          stroke-width="1.7"
        />
      </svg>
      <span
        v-if="store.unread > 0"
        class="absolute -right-1 -top-1 grid min-w-[16px] place-items-center rounded-full bg-red-500 px-1 text-[10px] font-bold leading-4 text-white"
      >
        {{ store.unread > 99 ? "99+" : store.unread }}
      </span>
    </button>

    <div
      v-if="open"
      class="absolute right-0 top-10 z-30 flex max-h-[70vh] w-80 flex-col overflow-hidden rounded-xl border border-slate-200 bg-white shadow-xl dark:border-slate-700 dark:bg-slate-900"
    >
      <div class="shrink-0 border-b border-slate-100 px-4 py-2.5 text-sm font-semibold text-slate-900 dark:border-slate-800 dark:text-white">
        平台通知
      </div>
      <div class="min-h-0 flex-1 overflow-y-auto">
        <p v-if="store.list.length === 0" class="px-4 py-8 text-center text-xs text-slate-400">
          暂无通知
        </p>
        <ul v-else class="divide-y divide-slate-100 dark:divide-slate-800">
          <li v-for="(n, i) in store.list" :key="`${n.file_name}-${n.received_at}-${i}`" class="px-4 py-3">
            <div class="flex items-start gap-2">
              <span class="mt-0.5 shrink-0 rounded px-1.5 py-0.5 text-[10px] font-semibold" :class="levelClass(n.level)">
                {{ levelLabel(n.level) }}
              </span>
              <span class="min-w-0 flex-1 self-center break-words text-sm font-medium text-slate-900 dark:text-white">
                {{ n.title }}
              </span>
              <span class="shrink-0 self-start font-mono text-[10px] text-slate-400">{{ formatTime(n.received_at) }}</span>
            </div>
            <p v-if="n.body" class="mt-1 whitespace-pre-wrap break-words text-xs leading-5 text-slate-500 dark:text-slate-400">
              {{ n.body }}
            </p>
            <p class="mt-1 text-[10px] text-slate-400">类型：平台通知 · 实例：{{ n.file_name }}</p>
          </li>
        </ul>
      </div>
    </div>
  </div>
</template>
