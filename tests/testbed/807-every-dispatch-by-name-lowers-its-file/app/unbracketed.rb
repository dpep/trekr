class Unbracketed
  class_eval <<~RUBY
    def go(n)
      __send__ n
    end
  RUBY

  def small_attrs = 1
end
