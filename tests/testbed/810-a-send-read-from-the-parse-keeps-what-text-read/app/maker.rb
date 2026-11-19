class Maker
  [:a].each do |kind|
    class_eval <<~RUBY
      def run_#{kind}
        Target.public_send("list_#{kind}")
      end
    RUBY
  end
end
