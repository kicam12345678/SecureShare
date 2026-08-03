import os
from typing import List

from fastapi import FastAPI, Request, UploadFile, File
from fastapi.responses import RedirectResponse, FileResponse
from fastapi.templating import Jinja2Templates
from fastapi.staticfiles import StaticFiles

app = FastAPI()

app.mount("/static", StaticFiles(directory="app/static"), name="static")

templates = Jinja2Templates(directory="app/templates")

os.makedirs("uploads", exist_ok=True)


@app.get("/")
def home(request: Request):
    return templates.TemplateResponse(
        "index.html",
        {"request": request}
    )


@app.get("/files")
def files(request: Request):
    file_list = os.listdir("uploads")

    return templates.TemplateResponse(
        "files.html",
        {
            "request": request,
            "files": file_list
        }
    )


@app.post("/upload")
async def upload_files(files: List[UploadFile] = File(...)):
    for file in files:

        with open(f"uploads/{file.filename}", "wb") as buffer:
            buffer.write(await file.read())

    return RedirectResponse(url="/files", status_code=303)


@app.get("/download/{filename}")
def download_file(filename: str):
    return FileResponse(
        path=f"uploads/{filename}",
        filename=filename
    )