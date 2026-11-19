class Underscored
  def build(name) = __send__(name)
  def small_attrs = 1
end

class Tried
  def build(name) = try(name)
  def small_attrs = 1
end

class Handled
  def build(name) = method(name).call
  def small_attrs = 1
end
