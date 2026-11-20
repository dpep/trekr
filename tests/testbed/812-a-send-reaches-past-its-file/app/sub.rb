class Sub < Base
  def other = 1
end
Sub.new.dispatch(:x)

class Unrelated
  def lonely = 1
end
