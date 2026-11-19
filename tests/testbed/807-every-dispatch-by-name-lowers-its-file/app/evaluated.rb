class Evaluated
  class_eval <<~RUBY
    def build(name) = public_send(name)
  RUBY

  def small_attrs = 1
end
