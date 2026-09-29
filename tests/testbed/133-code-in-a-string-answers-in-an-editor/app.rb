class Widget
  %w[alpha beta gamma].each do |n|
    class_eval <<~RUBY
      def #{n}_x
        total = 1
        total + 2
      end
    RUBY
  end
end
