module Http
  QUERY = %w[get head].freeze
end

class Connection
  Http::QUERY.each do |method|
    class_eval <<-RUBY, __FILE__, __LINE__ + 1
      def #{method}(url = nil)
        run_request(:#{method}, url)
      end
    RUBY
  end

  BODY = %i[post put].freeze

  BODY.each do |method|
    class_eval <<-RUBY, __FILE__, __LINE__ + 1
      def #{method}(url = nil, body = nil)
        run_request(:#{method}, url, body)
      end
    RUBY
  end

  def options(url)
    run_request(:options, url)
  end

  def run_request(verb, url, body = nil)
  end
end

Connection.new.put("/x")
Connection.new.get("/x")
