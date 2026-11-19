class Nested
  [:a].each do |n|
    class_eval <<~RUBY
      def go_#{n}
        instance_eval "send('foo_#{n}')"
      end
    RUBY
  end

  def foo_a = 1
end
