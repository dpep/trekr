class Chained
  def go = send(:aaa).send(:bbb)
  def aaa = self
  def bbb = 1
end
Chained.new.go
