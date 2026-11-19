class Spelled
  [:a].each do |n|
    class_eval <<~RUBY.strip
      def run_#{n}(o) = o.send("foo_#{n}")
    RUBY
  end

  def foo_a = 1
end
