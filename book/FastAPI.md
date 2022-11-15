# Step 1: Setup your Puff project

Follow the instructions on how to setup a new poetry project and install Puff:

* [Using the CLI](https://github.com/hansonkd/puff/blob/master/book/CLI.md)

# Step 2: Add a FastAPI app

Install FastAPI to your your project:

```
poetry add fastapi
```

Add your flask application in a new file `my_puff_project/fastapi.py`


```python
from fastapi import FastAPI


app = FastAPI()


@app.get("/fast-api")
async def read_root():
    return "<p>Hello, World!</p>"
```

Update `puff.toml` with the following lines

```toml
asyncio = true
asgi = "my_puff_project.fastapi.app"
```

Run the server:

```bash
poetry run puff serve
```

To use Puff in development mode, use `puff-watch` which is a wrapper around puff that will restart the process when files change in the directory.

```bash
poetry run puff-watch serve
# Spawn multiple processes on the same Port to simulate hypercorn workers
PUFF_REUSE_PORT=true poetry run puff-watch --num 2 serve
```

# Step 3: Add GraphQL

Create a new file called `my_puff_project/graphql.py`

```python
from typing import Optional, Iterable, Any
from dataclasses import dataclass
from puff.pubsub import global_pubsub as pubsub


@dataclass
class DbObject:
    was_input: int
    title: str
    

@dataclass
class Query:
    @classmethod
    def hello_world(cls, parents, context, /, my_input: int) -> tuple[list[DbObject], str, list[Any]]:
        # Return a Raw query for Puff to execute in Postgres.
        # The ellipsis is a placeholder allowing the Python type system to know which field type it should transform into.
        return ..., "SELECT $1::int as was_input, \'hi from pg\'::TEXT as title", [my_input]

    @classmethod
    def auth_token(cls, context, /) -> str:
        # All GraphQL queries have access to the Bearer token if set.
        return context.auth_token()
    
    @classmethod
    def new_connection_id(cls, context, /) -> str:
        # Get a new connection identifier for pubsub.
        return pubsub.new_connection_id()


@dataclass
class Mutation:
    @classmethod
    def send_message_to_channel(cls, context, /, connection_id: str, channel: str, message: str) -> bool:
        return pubsub.publish_as(connection_id, channel, message)


@dataclass
class MessageObject:
    message_text: str
    from_connection_id: str
    num_processed: int


@dataclass
class Subscription:
    @classmethod
    def read_messages_from_channel(cls, context, /, channel: str, connection_id: Optional[str] = None) -> Iterable[MessageObject]:
        if connection_id is not None:
            conn = pubsub.connection_with_id(connection_id)
        else:
            conn = pubsub.connection()
        conn.subscribe(channel)
        num_processed = 0
        while msg := conn.receive():
            from_connection_id = msg.from_connection_id
            # Filter out messages from yourself.
            if connection_id != from_connection_id:
                yield MessageObject(message_text=msg.text, from_connection_id=from_connection_id, num_processed=num_processed)
                num_processed += 1


@dataclass
class Schema:
    query: Query
    mutation: Mutation
    subscription: Subscription
```

Update `puff.toml` with the following lines

```toml
[[postgres]]
enable = true

[[pubsub]]
enable = true

[[graphql]]
url = "/graphql/"
subscription_url = "/subscriptions/"
playground_url = "/playground/"
database = "default"
```

Run the server and go to `http://localhost:7777/playground/`


# Step 4: Add Distributed Tasks

Update `puff.toml` with the following lines

```toml
[[task_queue]]
enable = true
```

Create a new file called `my_puff_project/tasks.py`

```python
from puff.task_queue import task

@task
def my_task(payload):
    print(f"Inside `my_task` with payload: {payload}")
    return {"from_task": payload}
```

Change your `Mutation` class in `graphql.py` to call the function through the queue:

```python
from puff.task_queue import global_task_queue as tq
from my_puff_project.tasks import my_task

...

@dataclass
class Mutation:
    @classmethod
    def send_message_to_channel(cls, context, /, connection_id: str, channel: str, message: str) -> bool:
        tq.add_task(my_task, {"auth_token": context.auth_token(), "connection_id": connection_id, "channel": channel, "message": message})
        return pubsub.publish_as(connection_id, channel, message)
```

Run a worker server by using wait_forever in another tab:

```bash
poetry run puff wait_forever
```

Now run the server. Going to `http://localhost:7777/graphql/` calling the mutation will trigger tasks.

```bash
poetry run puff serve
```
