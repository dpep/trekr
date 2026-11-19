class Twice
  def go = send(:b1)
  def run(list) = list.each(&method(:b1))
  def b1(x = nil) = x
end
Twice.new.go
Twice.new.run([])
