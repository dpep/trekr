class Stripped
  class_eval <<~RUBY.strip
    def build(name) = public_send(name)
  RUBY

  def small_attrs = 1
end
