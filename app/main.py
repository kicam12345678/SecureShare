import os
import zipfile
import tempfile
import shutil
import json

from datetime import datetime
from typing import List

from fastapi import FastAPI, Request, UploadFile, File, Form
from fastapi.responses import RedirectResponse, FileResponse, JSONResponse
from fastapi.staticfiles import StaticFiles
from fastapi.templating import Jinja2Templates

app = FastAPI()

app.mount("/static", StaticFiles(directory="app/static"), name="static")

templates = Jinja2Templates(directory="app/templates")

UPLOAD_DIR = "uploads"
TRASH_DIR = os.path.join(UPLOAD_DIR, ".trash")
TRASH_META = os.path.join(TRASH_DIR, "trash.json")

os.makedirs(UPLOAD_DIR, exist_ok=True)


def ensure_trash():
    # Ensure trash folder and metadata exist
    os.makedirs(TRASH_DIR, exist_ok=True)
    if not os.path.exists(TRASH_META):
        with open(TRASH_META, "w", encoding="utf-8") as f:
            json.dump({}, f, ensure_ascii=False, indent=2)


def load_trash_metadata():
    ensure_trash()
    with open(TRASH_META, "r", encoding="utf-8") as f:
        try:
            return json.load(f)
        except json.JSONDecodeError:
            return {}


def save_trash_metadata(data):
    ensure_trash()
    with open(TRASH_META, "w", encoding="utf-8") as f:
        json.dump(data, f, ensure_ascii=False, indent=2)


def unique_name(path):
    if not os.path.exists(path):
        return path
    base, ext = os.path.splitext(path)
    counter = 1
    while True:
        candidate = f"{base}_{counter}{ext}"
        if not os.path.exists(candidate):
            return candidate
        counter += 1


def unique_trash_name(name):
    candidate = name
    if not os.path.exists(os.path.join(TRASH_DIR, candidate)):
        return candidate
    base, ext = os.path.splitext(name)
    timestamp = datetime.now().strftime("%Y%m%d%H%M%S")
    candidate = f"{base}_{timestamp}{ext}"
    if not os.path.exists(os.path.join(TRASH_DIR, candidate)):
        return candidate
    counter = 1
    while True:
        candidate = f"{base}_{timestamp}_{counter}{ext}"
        if not os.path.exists(os.path.join(TRASH_DIR, candidate)):
            return candidate
        counter += 1


def get_dir_size(path):
    total = 0
    for root, _, files in os.walk(path):
        for filename in files:
            total += os.path.getsize(os.path.join(root, filename))
    return total


def is_descendant(path, parent):
    path = os.path.abspath(path)
    parent = os.path.abspath(parent)
    try:
        return os.path.commonpath([path, parent]) == parent and path != parent
    except ValueError:
        return False


def move_to_trash(filename: str):
    ensure_trash()
    src = os.path.join(UPLOAD_DIR, filename)
    if not os.path.exists(src):
        return

    trash_name = unique_trash_name(os.path.basename(filename))
    dest = os.path.join(TRASH_DIR, trash_name)
    shutil.move(src, dest)

    metadata = load_trash_metadata()
    metadata[trash_name] = {
        "original": filename,
        "deleted_at": datetime.now().strftime("%Y-%m-%d %H:%M"),
        "is_dir": os.path.isdir(dest)
    }
    save_trash_metadata(metadata)


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
        # Hide trash folder from normal listing
        if filename == ".trash":
            continue

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


@app.post("/rename")
def rename(
    old_name: str = Form(...),
    new_name: str = Form(...),
    path: str = Form("")
):

    old_path = os.path.join(
        UPLOAD_DIR,
        path,
        old_name
    )

    new_path = os.path.join(
        UPLOAD_DIR,
        path,
        new_name
    )

    if os.path.exists(old_path):

        os.rename(old_path, new_path)

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


@app.get("/delete/{filename:path}")
def delete_file(filename: str):

    # Move deleted item to trash instead of permanently deleting it
    move_to_trash(filename)

    parent = os.path.dirname(filename)

    return RedirectResponse(
        url=f"/files?path={parent}",
        status_code=303
    )


@app.get("/folders")
def list_folders():

    # Return all existing folder paths under uploads, excluding trash
    folder_paths = [""]

    for root, dirs, _ in os.walk(UPLOAD_DIR):
        if os.path.abspath(root).startswith(os.path.abspath(TRASH_DIR)):
            continue
        rel_root = os.path.relpath(root, UPLOAD_DIR)
        if rel_root == ".":
            rel_root = ""
        for d in dirs:
            folder = os.path.join(rel_root, d).replace("\\", "/")
            if folder.startswith(".trash"):
                continue
            folder_paths.append(folder)

    folder_paths = sorted(set(folder_paths))
    return JSONResponse(folder_paths)


@app.post("/move")
def move_item(
    item_name: str = Form(...),
    path: str = Form(""),
    dest: str = Form(...)
):

    # Move a file or folder to a selected destination folder
    source = os.path.join(UPLOAD_DIR, path, item_name)
    dest_dir = os.path.join(UPLOAD_DIR, dest)

    if not os.path.exists(source) or not os.path.isdir(dest_dir):
        return RedirectResponse(url=f"/files?path={path}", status_code=303)

    # Prevent moving a folder into itself or its descendant
    if is_descendant(dest_dir, source):
        return RedirectResponse(url=f"/files?path={path}", status_code=303)

    os.makedirs(dest_dir, exist_ok=True)

    dest_path = unique_name(os.path.join(dest_dir, item_name))
    shutil.move(source, dest_path)

    return RedirectResponse(url=f"/files?path={path}", status_code=303)


@app.post("/copy")
def copy_item(
    item_name: str = Form(...),
    path: str = Form(""),
    dest: str = Form(...)
):

    # Copy a file or folder to a selected destination folder
    source = os.path.join(UPLOAD_DIR, path, item_name)
    dest_dir = os.path.join(UPLOAD_DIR, dest)

    if not os.path.exists(source) or not os.path.isdir(dest_dir):
        return RedirectResponse(url=f"/files?path={path}", status_code=303)

    os.makedirs(dest_dir, exist_ok=True)

    dest_path = unique_name(os.path.join(dest_dir, item_name))
    if os.path.isdir(source):
        shutil.copytree(source, dest_path)
    else:
        shutil.copy2(source, dest_path)

    return RedirectResponse(url=f"/files?path={path}", status_code=303)


@app.post("/paste")
def paste_item(
    item_name: str = Form(...),
    action: str = Form(...),
    path: str = Form(""),
    dest: str = Form(...)
):

    # Perform cut/paste or copy/paste based on clipboard action
    if action == "cut":
        return move_item(item_name=item_name, path=path, dest=dest)
    elif action == "copy":
        return copy_item(item_name=item_name, path=path, dest=dest)

    return RedirectResponse(url=f"/files?path={path}", status_code=303)


@app.get("/trash")
def trash(request: Request):

    metadata = load_trash_metadata()
    items = []

    for trash_name, info in metadata.items():
        trash_path = os.path.join(TRASH_DIR, trash_name)
        if not os.path.exists(trash_path):
            continue

        size = get_dir_size(trash_path) if info.get("is_dir") else os.path.getsize(trash_path)
        size_text = f"{size} B" if size < 1024 else f"{size/1024:.2f} KB" if size < 1024*1024 else f"{size/(1024*1024):.2f} MB"

        items.append({
            "trash_name": trash_name,
            "name": os.path.basename(info.get("original", trash_name)),
            "original": info.get("original", ""),
            "deleted_at": info.get("deleted_at", ""),
            "is_dir": info.get("is_dir", False),
            "size": size_text
        })

    return templates.TemplateResponse(
        "trash.html",
        {
            "request": request,
            "items": items
        }
    )


@app.post("/trash/restore")
def restore_trash(
    trash_name: str = Form(...)
):

    metadata = load_trash_metadata()
    if trash_name not in metadata:
        return RedirectResponse(url="/trash", status_code=303)

    info = metadata[trash_name]
    trash_path = os.path.join(TRASH_DIR, trash_name)
    original_path = os.path.join(UPLOAD_DIR, info.get("original", ""))
    os.makedirs(os.path.dirname(original_path), exist_ok=True)
    restore_path = unique_name(original_path) if os.path.exists(original_path) else original_path

    if os.path.exists(trash_path):
        shutil.move(trash_path, restore_path)

    del metadata[trash_name]
    save_trash_metadata(metadata)

    return RedirectResponse(url="/trash", status_code=303)


@app.post("/trash/delete")
def delete_from_trash(
    trash_name: str = Form(...)
):

    metadata = load_trash_metadata()
    trash_path = os.path.join(TRASH_DIR, trash_name)

    if os.path.exists(trash_path):
        if os.path.isdir(trash_path):
            shutil.rmtree(trash_path)
        else:
            os.remove(trash_path)

    if trash_name in metadata:
        del metadata[trash_name]
        save_trash_metadata(metadata)

    return RedirectResponse(url="/trash", status_code=303)


@app.post("/trash/empty")
def empty_trash():

    metadata = load_trash_metadata()

    for trash_name in list(metadata.keys()):
        trash_path = os.path.join(TRASH_DIR, trash_name)
        if os.path.exists(trash_path):
            if os.path.isdir(trash_path):
                shutil.rmtree(trash_path)
            else:
                os.remove(trash_path)
        del metadata[trash_name]

    save_trash_metadata(metadata)

    return RedirectResponse(url="/trash", status_code=303)


@app.get("/preview/{filename:path}")
def preview(filename: str):
    full_path = os.path.join(UPLOAD_DIR, filename)
    return FileResponse(full_path)