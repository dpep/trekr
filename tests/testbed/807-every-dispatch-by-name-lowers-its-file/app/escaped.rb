class Escaped
  class_eval <<~RUBY
    def go(k)
      public_send("\#{k}")
    end
  RUBY

  def small_attrs = 1
end
