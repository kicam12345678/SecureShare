import os
import zipfile
import zipfile
import tempfile
import shutil

from datetime import datetime
from typing import List

from fastapi import FastAPI, Request, UploadFile, File, Form
from fastapi.responses import RedirectResponse, FileResponse
from fastapi.staticfiles import StaticFiles
from fastapi.templating import Jinja2Templates

app = FastAPI()

app.mount("/static", StaticFiles(directory="app/static"), name="static")

templates = Jinja2Templates(directory="app/templates")

UPLOAD_DIR = "uploads"

os.makedirs(UPLOAD_DIR, exist_ok=True)


@app.get("/")
def home(request: Request):
    return templates.TemplateResponse(
        "index.html",
        {"request": request}
    )


@app.get("/files")
def files(request: Request, path: str = ""):

    current_path = os.path.join(UPLOAD_DIR, path)

    if not os.path.exists(current_path):
        current_path = UPLOAD_DIR
        path = ""

    file_list = []

    for filename in sorted(os.listdir(current_path)):

        full_path = os.path.join(current_path, filename)

        size_text = "-"

        if os.path.isfile(full_path):

            size = os.path.getsize(full_path)

            if size < 1024:
                size_text = f"{size} B"

            elif size < 1024 * 1024:
                size_text = f"{size/1024:.2f} KB"

            else:
                size_text = f"{size/(1024*1024):.2f} MB"

        upload_time = datetime.fromtimestamp(
            os.path.getmtime(full_path)
        ).strftime("%Y-%m-%d %H:%M")

        file_list.append({

            "name": filename,

            "size": size_text,

            "date": upload_time,

            "is_dir": os.path.isdir(full_path)

        })

    parent = ""

    if path:

        parent = os.path.dirname(path)

    return templates.TemplateResponse(

        "files.html",

        {

            "request": request,

            "files": file_list,

            "current_path": path,

            "parent": parent

        }

    )


@app.post("/upload")
async def upload_files(
    files: List[UploadFile] = File(...),
    path: str = Form("")
):

    save_dir = os.path.join(UPLOAD_DIR, path)

    os.makedirs(save_dir, exist_ok=True)

    for file in files:

        save_path = os.path.join(
            save_dir,
            file.filename
        )

        with open(save_path, "wb") as buffer:
            buffer.write(await file.read())

    return RedirectResponse(
        url=f"/files?path={path}",
        status_code=303
    )


@app.post("/create-folder")
def create_folder(
    folder_name: str = Form(...),
    path: str = Form("")
):

    folder_path = os.path.join(
        UPLOAD_DIR,
        path,
        folder_name
    )

    os.makedirs(folder_path, exist_ok=True)

    return RedirectResponse(
        url=f"/files?path={path}",
        status_code=303
    )


@app.get("/download/{filename:path}")
def download_file(filename: str):

    full_path = os.path.join(
        UPLOAD_DIR,
        filename
    )

    return FileResponse(
        full_path,
        filename=os.path.basename(filename)
    )


@app.get("/download-folder/{folder:path}")
def download_folder(folder: str):

    folder_path = os.path.join(UPLOAD_DIR, folder)

    temp_dir = "temp"

    os.makedirs(temp_dir, exist_ok=True)

    zip_name = os.path.basename(folder) + ".zip"

    zip_path = os.path.join(temp_dir, zip_name)

    with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_DEFLATED) as zipf:

        for root, dirs, files in os.walk(folder_path):

            for file in files:

                full_path = os.path.join(root, file)

                arcname = os.path.relpath(full_path, folder_path)

                zipf.write(full_path, arcname)

    return FileResponse(
        zip_path,
        filename=zip_name,
        media_type="application/zip"
    )


def delete_file(filename: str):

    full_path = os.path.join(
        UPLOAD_DIR,
        filename
    )

    if os.path.isfile(full_path):

        os.remove(full_path)

    elif os.path.isdir(full_path):

        try:
            os.rmdir(full_path)
        except OSError:
            pass

    parent = os.path.dirname(filename)

    return RedirectResponse(
        url=f"/files?path={parent}",
        status_code=303
    )
@app.get("/download-folder/{folder:path}")
def download_folder(folder: str):

    folder_path = os.path.join(UPLOAD_DIR, folder)

    if not os.path.isdir(folder_path):
        return RedirectResponse("/files", status_code=303)

    temp_dir = tempfile.mkdtemp()

    zip_name = os.path.basename(folder_path) + ".zip"

    zip_path = os.path.join(temp_dir, zip_name)

    with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_DEFLATED) as zipf:

        for root, dirs, files in os.walk(folder_path):

            for file in files:

                full_path = os.path.join(root, file)

                arcname = os.path.relpath(full_path, folder_path)

                zipf.write(full_path, arcname)

    return FileResponse(
        zip_path,
        filename=zip_name,
        media_type="application/zip"
    )