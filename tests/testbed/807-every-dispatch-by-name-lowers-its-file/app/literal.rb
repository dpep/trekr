class Literal
  def build = send(:used)
  def used = 1
  def unused = 1
end
