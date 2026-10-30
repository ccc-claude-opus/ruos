This is your home directory. Files you make here live in this boot's
writable layer, on top of the read-only root image. Type `save` to commit
them into the image, so the next boot has them too.

Try:
  ls -l /
  cat poem.txt | grep sea | wc
  echo hello > note.txt
  cat note.txt
