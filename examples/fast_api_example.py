from fastapi import FastAPI

app = FastAPI()


@app.get("/fast-api")
def read_root():
    return {"Hello": "World", "from": "Fast API"}
