class Bare
  class_eval <<~RUBY
    def go(n)
      send n
    end
  RUBY

  def small_attrs = 1
end
