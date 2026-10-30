# count.sh N: counts from 1 to N and says which numbers are even.
n=$1
if [ -z "$n" ]; then n=3; fi
i=1
while [ $i -le $n ]; do
  if [ $((i % 2)) -eq 0 ]; then
    echo "$i is even"
  else
    echo "$i is odd"
  fi
  i=$((i + 1))
done
echo "counted to $n"
