from greenlet import greenlet
import dataclasses
import contextvars
from threading import Thread
from typing import Any
import queue

parent_thread = contextvars.ContextVar("parent_thread")
greenlet_context = contextvars.ContextVar("greenlet_context")


@dataclasses.dataclass(frozen=True, slots=True)
class Task:
    args: list
    kwargs: dict
    ret_func: Any
    task_function: Any
    context: Any

    def process(self):
        new_greenlet = greenlet(self.task_function)
        new_greenlet.switch(self.args, self.kwargs, self.ret_func, self.context)


@dataclasses.dataclass(frozen=True, slots=True)
class Result:
    greenlet: Any
    result: Any

    def process(self):
        self.greenlet.switch(self.result)


class MainThread(Thread):
    def __init__(self, event_queue, on_thread_start=None):
        self.event_queue = event_queue
        # self.task_spawner = task_spawner
        self.on_thread_start = on_thread_start
        self.main_greenlet = None
        self.event_loop_processor = None
        self.read_from_queue_processor = None
        super().__init__()

    def run(self):
        if self.on_thread_start is not None:
            self.on_thread_start()
        self.main_greenlet = greenlet.getcurrent()
        self.event_loop_processor = greenlet(self.loop_commands)
        self.read_from_queue_processor = greenlet(self.read_from_queue)
        self.event_loop_processor.switch()
        while True:
            self.read_from_queue_processor.switch()

    def spawn(self, task_function, context, args, kwargs, ret_func):
        task_function_wrapped = self.generate_spawner(task_function)
        task = Task(args=args, kwargs=kwargs, ret_func=ret_func, task_function=task_function_wrapped, context=context)
        self.event_queue.put(task)

    def return_result(self, greenlet, result_value):
        result_event = Result(greenlet=greenlet, result=result_value)
        self.event_queue.put(result_event)

    def generate_spawner(self, func):
        def override_spawner(args, kwargs, ret_func, context):
            parent_thread.set(self)
            greenlet_context.set(context)
            ret_func(func(*args, **kwargs))
        return override_spawner

    def loop_commands(self):
        while True:
            event = self.read_next_event()
            event.process()

    def read_from_queue(self):
        while True:
            event = self.event_queue.get()
            if event is None:
                break
            self.event_loop_processor.switch(event)

    def read_next_event(self):
        res = self.main_greenlet.switch("blocking here")
        return res


def start_event_loop():
    q = queue.Queue()

    loop_thread = MainThread(q)
    loop_thread.start()

    return loop_thread


def wrap_async(f):
    sub_job = greenlet.getcurrent()
    thread = parent_thread.get()
    context = greenlet_context.get()

    def return_result(result):
        thread.return_result(sub_job, result)

    f(context, return_result)

    return thread.event_loop_processor.switch()


def get_redis():
    pass
